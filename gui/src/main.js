// The cut editor, arranged after TMPGEnc MPEG Smart Renderer 6's, and, like
// the reference tool's, a window of its own.
//
// It is opened on one clip out of the list window and left again with OK or
// キャンセル. It never chooses the clip and never decides what becomes of the
// cuts: `app.js` does both. All this window knows how to do is show a
// recording and let it be cut, and it hands what it did back over the wire
// (`editor-state`) as the cuts happen rather than only at the end -- so
// closing it by the title bar's cross loses nothing.
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
const emit = T.event && T.event.emit;
const dialog = T.dialog;
const jlog = (m) => invoke && invoke("log", { msg: String(m) });
jlog("main.js start");

import { fmt, chLabel, cmNote, noBrowserMenu, noNativeDrag } from "./shared.js";
import { t as tr, applyStatic, setLang, onLangChange, confirmWithOs } from "./i18n.js";
import * as prefs from "./prefs.js";

const el = (id) => document.getElementById(id);
const track = el("track");
const ctx = track.getContext("2d");

let src = null;
/// What the recording is called, when that is not its file name: a
/// recording on a BDAV disc is named by the disc's index, and the file it
/// is in is called `00001.m2ts`.
let shownName = null;
/// Where a mark file for this recording would be, without an extension. What
/// the list window worked out (`side` at `editor-open`): beside the disc for
/// a recording on one, because inside an image there is nothing to be beside.
let sideBase = null;
let playhead = 0; // source time, always on material that still exists
let selA = 0; // selection, in output time
let selB = 0;
let dragging = null;
let cuts = []; // source ranges taken out
let keeps = []; // [{a, b, at}] source ranges that survive, with output offset
let gops = []; // output times where a GOP starts
let seams = []; // output times of the joins in `joinTimes()`
let outDur = 0;
let keyframes = []; // source times
let activeKey = null; // source time of the selected mark, null for none
/// The chapter points the disc this recording came off carries, on the
/// stream's own clock. Put down when the recording is opened; kept because
/// they are the one source of marks that cannot be asked for again -- the
/// disc is not read a second time from in here. Empty for a plain file.
let discChapters = [];
let cmBlocks = [];
/// The sentence under the last detection, kept so it can travel back to the
/// list with the rest of the state -- the row there says what was found, and
/// a detection run in here has to reach it.
let cmSummary = "";
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
/// Source time of the picture on the stage, or -1 for none. What `paintFast`
/// weighs its stand-in against before putting it up.
let shownTime = -1;
let stripToken = 0;
let hoverToken = 0;
let stripShots = [];
let stripCache = null;
/// Mark time -> a promise for that mark's picture. Promises rather than URLs
/// so that a re-render during a decode joins the decode already running.
const cardThumbs = new Map();
/// Mark time -> the guess standing in for it until the walk lands, by the same
/// key. Kept rather than only painted: the walk landing renders the list
/// again, and a card that has something in it must not go back to blank on the
/// way to having the right thing in it.
const cardGuesses = new Map();

const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
const frame = () => (src && src.fps > 0 ? 1 / src.fps : 1 / 30);
const frameNo = (t) => Math.round(t * (src ? src.fps : 30));

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

/// Where the material begins, which is where frame 0 is.
///
/// The first picture that can be decoded rather than the container's zero: a
/// broadcast recording frequently opens most of a second in, and the timeline
/// starts at the picture and not at the clock.
///
/// The walk's list of access points is the exact answer and `head` is the
/// same picture read off the front of the file, which is what there is to go
/// on while the walk is still reading. Before either -- a file whose opening
/// held no picture to find -- the clock's own zero, as before.
const headTime = () => (src ? (src.points.length ? src.points[0] : (src.head ?? 0)) : 0);

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
  let pos = headTime();
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

// --- the history --------------------------------------------------------
//
// 取消 and やり直し step through the *operations*, not through the cuts
// alone. Somebody who cuts, looks at the join and decides the cut was wrong
// wants back what they had when they made it -- and what they had includes
// the selection they cut by, which is the thing they are about to adjust and
// cut again. Taking the cut out and leaving the selection collapsed on the
// join made them mark the range a second time to change it by a frame.
//
// So a step carries the whole of what an edit touches: the cuts, the marks,
// which mark was picked out, the selection and the playhead. Putting one back
// puts all of it back.

/// Where the state was before each edit, oldest first.
let past = [];
/// The states 取消 stepped out of, newest last. Emptied by the next edit:
/// stepping back and then cutting again is a new course, and what was ahead
/// on the old one cannot be reached from here.
let undone = [];

/// Everything an edit can touch.
///
/// `cuts` is copied a range at a time rather than by `slice`: the ranges
/// themselves are edited in place by `normalise`, so a shallow copy of the
/// list would hand the past a range the present goes on to move.
const snapshot = () => ({
  cuts: cuts.map((c) => ({ a: c.a, b: c.b })),
  keyframes: keyframes.slice(),
  activeKey,
  selA,
  selB,
  playhead,
});

/// What a recording arrives with is not something that was done to it.
///
/// The marks beside the file, the chapters off the disc, the blocks a
/// detection the list ran found -- all of them land on the timeline while the
/// recording is being opened, and not one of them is an edit. Counted as one,
/// 取消 sat lit on a window nobody had touched yet, and the first press of it
/// threw away the marks the window had opened with. `remember` says nothing
/// for as long as this is set.
let settling = 0;

/// Put a recording's own marks down without writing them into the history.
async function settle(fn) {
  settling += 1;
  try {
    return await fn();
  } finally {
    settling -= 1;
  }
}

/// Put down where we are, on the way into an edit.
function remember() {
  if (settling) return;
  past.push(snapshot());
  if (past.length > 50) past.shift();
  undone = [];
  paintHistory();
}

function paintHistory() {
  el("undo-cut").disabled = past.length === 0;
  el("redo-cut").disabled = undone.length === 0;
}

/// One step through the history, either way: 取消 goes back through `past`,
/// やり直し forward through `undone`.
///
/// What is on screen goes on the other pile on the way out, and it is taken
/// here rather than when the edit was made: the selection and the playhead go
/// on moving between edits, and 取消 followed by やり直し has to land where
/// it started, not where the cut left things.
function stepHistory(from, to) {
  if (!from.length) return;
  if (playing) stopPlay();
  to.push(snapshot());
  const back = from.pop();
  cuts = back.cuts.map((c) => ({ a: c.a, b: c.b }));
  keyframes = back.keyframes.slice();
  activeKey = back.activeKey;
  afterCutsChanged(back);
  showFrame(playhead);
}

function applyCuts(next) {
  remember();
  cuts = normalise(next);
  afterCutsChanged();
}

/// Source times where the timeline has closed over a cut. Cutting the head of
/// the recording leaves only one segment and so no *internal* join -- but its
/// new beginning is a join like any other, and counts here.
function joinTimes() {
  const list = keeps.slice(1).map((k) => k.a);
  const head = headTime();
  if (keeps.length && keeps[0].a > head + frame() / 2) list.push(keeps[0].a);
  return list.sort((a, b) => a - b);
}

/// `back` is the state being put back, when this is a step through the
/// history rather than an edit. It says where to leave the playhead and the
/// selection, and it has already said which mark is picked out.
function afterCutsChanged(back = null) {
  // The list is told about every cut as it happens, not at OK; see `sync`.
  sync();
  const before = playOut();
  const had = joinTimes();
  rebuildTimeline();
  // Every join a cut leaves behind is worth a mark: it is exactly the place
  // you will want to come back to and check.
  const joins = joinTimes();
  // The join this edit just opened is the place to look, so select its mark --
  // whether the join brought the mark with it or landed on one that was
  // already there, as it does when you cut a detected break out. A step
  // through the history picks nothing out here: the state being put back
  // carries the mark that was picked out when it was current.
  if (!back) {
    const fresh = joins.filter((t) => !had.some((o) => Math.abs(o - t) < frame() / 2));
    if (fresh.length) activeKey = fresh[fresh.length - 1];
  }
  const all = keyframes.concat(joins).sort((a, b) => a - b);
  keyframes = all.filter((t, i) => i === 0 || t - all[i - 1] > frame() / 2);
  paintHistory();
  playhead = outToSrc(clamp(back ? srcToOutSeam(back.playhead) : before, 0, outDur));
  selA = clamp(back ? back.selA : selA, 0, outDur);
  selB = clamp(back ? back.selB : selB, selA, outDur);
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

/// A guess at each mark's picture, for while the walk is still running.
///
/// The same seek the strip makes then: the container's own, which lands near
/// the instant rather than on it. One call for all of them, because the walk
/// is reading the same recording and over a share an open apiece is the
/// difference between cards that fill and a walk that stalls.
///
/// Not written into `cardThumbs`. That cache is the frame a card says it is
/// showing, and this is not that frame -- it is what stands there until there
/// is one.
async function paintGuesses(times, imgs) {
  const want = times.map((_, i) => i).filter((i) => !cardThumbs.has(cardKey(times[i])));
  if (!want.length) return;
  let got;
  try {
    got = await invoke("glimpses", { path: src.path, times: want.map((i) => times[i]), width: 200 });
  } catch (e) {
    jlog(`glimpses for the cards: ${e}`);
    return;
  }
  want.forEach((i, k) => {
    const shot = got[k];
    if (!shot) return;
    cardGuesses.set(cardKey(times[i]), shot.url);
    if (!imgs[i].dataset.exact) imgs[i].src = shot.url;
  });
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
  // Until the walk lands there is nothing open on the far side to decode an
  // exact frame from: the ask comes back empty, and the cards sat blank for
  // the whole of it -- a second a gigabyte, and longer over a share. The
  // guide tells you to go ahead and open a recording that is still being
  // read, and the film strip fills itself from the container's own guess
  // while it is; the cards were the one part of the window that did not.
  // They do now, and `pointsArrived` renders again, at which point the frame
  // the card says it is showing replaces the guess.
  if (!walked()) {
    paintGuesses(times, imgs);
    return;
  }
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
      cardGuesses.delete(cardKey(t));
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
  const next = all.filter((t, i) => i === 0 || t - all[i - 1] > frame() / 2);
  // A mark is not a cut, but it is still something that was done, and 取消
  // steps back through what was done. A press that puts nothing down -- the
  // same frame marked twice -- is not something that was done.
  if (next.length !== keyframes.length) remember();
  keyframes = next;
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
  // Runs whenever the marks change, and on a bare selection change too --
  // which `sync` coalesces away.
  sync();
  const list = el("keyframes");
  const live = liveKeyframes();
  el("key-count").textContent = live.length ? tr("editor.keyCount", { n: live.length }) : "";
  list.innerHTML = "";
  if (!live.length) {
    const p = document.createElement("div");
    p.className = "clips-empty";
    p.textContent = tr("editor.keyframes.empty");
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
    // Whatever was already standing in for this mark, before anything is
    // asked for: this list is rebuilt from nothing every time it is drawn,
    // and the walk landing draws it again.
    const guess = cardGuesses.get(cardKey(t));
    if (guess) img.src = guess;
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
    kill.title = tr("editor.keyframes.kill");
    kill.addEventListener("click", (ev) => {
      ev.stopPropagation();
      remember();
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

/// Whether the walk has been over this recording, so the access points are
/// known and everything that needs them can be exact.
const walked = () => !!src && src.points.length > 0;

/// A picture for the stage.
///
/// `preview` is the exact one and needs the access points: it seeks to the
/// entry point before the instant and decodes forward to it. Before the walk
/// there are no entry points, so the container is asked for its own guess
/// instead -- one open, one GOP, and a landing that can be a second or two
/// out on a transport stream. The picture answers with its own instant, and
/// the frame counter follows that rather than the pointer, so what is on
/// screen and what is written under it always agree.
const stagePicture = (at) =>
  walked()
    ? invoke("preview", { time: at, width: stageWidth() })
    : invoke("glimpse", { path: src.path, time: at, width: stageWidth() });

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
      shot = await stagePicture(ask);
      if (token !== previewToken) return;
      if (srcToOut(shot.time) !== null) break;
    }
    // Snap to the picture that actually came back, so the frame counter and
    // the picture never disagree. They would under 2:3 pulldown, where the
    // pictures do not sit on the 29.97 fps grid the playhead moves along.
    //
    // **Only where the picture is the frame that was asked for**, which is to
    // say only once the walk has found the access points. Before that the
    // stage is filled by `glimpse`, and a glimpse lands on an entry point
    // near the instant rather than on it: measured a hundred seconds into
    // four recordings, five to fifteen frames past the frame asked for on
    // broadcast material and thirty-nine on the one with the longest GOPs.
    //
    // Snapping the pointer onto that is what stepping a frame at a time ran
    // into during the walk. Forward, the press asked for one frame on, got
    // the entry point a GOP further on and moved the pointer *there*, so a
    // press was fifteen frames and the press after it another fifteen. Back
    // was worse: the entry point nearest the frame behind is the same one
    // ahead, so the button that means "one frame back" went forwards.
    //
    // So during the walk the pointer keeps the frame it was moved to and the
    // stage carries the nearest picture there is, which the overlay says in
    // as many words. `pointsArrived` asks again for the frame the pointer is
    // really on, and from there the two agree exactly.
    const exact = walked();
    if (exact && srcToOut(shot.time) !== null) playhead = shot.time;
    updateReadouts();
    draw();
    el("preview").src = shot.url;
    shownTime = shot.time;
    const onPoint = exact && atPoint(shot.time);
    el("ovl-kind").textContent = tr(
      onPoint
        ? "editor.frameKindPoint"
        : exact
          ? "editor.frameKind"
          : "editor.frameKindNear",
      { kind: shot.kind }
    );
    el("ovl-kind").className = onPoint ? "key" : "";
    showSubs(shot.time);
  } catch (e) {
    if (token === previewToken) el("status").textContent = tr("editor.previewFailed", { e });
  }
  scheduleStrip();
}

// --- subtitles over the picture -------------------------------------------
//
// Off unless it is asked for. A cutting screen is for the picture: what a
// caption tells you is where a line begins and ends, which matters at the
// two or three instants a cut is being placed near one and is in the way
// everywhere else.
//
// The backend answers "what is on screen at this instant" and holds a
// stretch of the subtitles decoded around wherever it was last asked, so
// this can ask on every frame -- while scrubbing and while playing -- and
// pay for a read only when the playhead leaves that stretch. See
// `subs.rs`.

/// The track being drawn, by the number the recording names it with, or
/// null for 表示しない.
let subsId = null;
/// What the backend last said is on screen, kept so that a resized window
/// can be redrawn without asking again.
let subsShown = null;
let subsToken = 0;
/// One question in flight at a time, and the last instant asked for always
/// answered. A drag asks on every pointer move; most are a lookup in the
/// stretch already read, but the one that leaves it costs a seek and a read
/// -- and without this the asks behind it queue up and land one after
/// another once the hand has stopped, each drawing a caption for a place the
/// playhead has left. See `subs.rs`.
let subsBusy = false;
let subsWanted = null;

/// Fill the picker from what the recording carries, and put it away where
/// it carries nothing.
function paintSubsPicker() {
  const pick = el("subs-pick");
  const sel = el("subs-track");
  if (!pick || !sel) return;
  const tracks = (src && src.subtitles) || [];
  pick.hidden = tracks.length === 0;
  if (!tracks.length) {
    subsId = null;
    sel.innerHTML = "";
    clearSubs();
    return;
  }
  // Rebuilt rather than patched: this is drawn once per recording, and the
  // answer it is holding belongs to the recording before it.
  sel.innerHTML = "";
  const off = document.createElement("option");
  off.value = "";
  off.textContent = tr("subs.off");
  sel.appendChild(off);
  for (const t of tracks) {
    const o = document.createElement("option");
    o.value = String(t.id);
    o.textContent = subsLabel(t, tracks);
    sel.appendChild(o);
  }
  // Kept across a reopen of the same recording, and only then: the editor is
  // opened on one clip at a time and remembering the language between two of
  // them would be remembering a track number that means something else.
  const still = tracks.some((t) => t.id === subsId);
  subsId = still ? subsId : null;
  sel.value = still ? String(subsId) : "";
  if (!still) clearSubs();
  // Unless 環境設定 says to start with them up, in which case the first track
  // is the one put up: which of several a recording carries is a question
  // only the person cutting can answer, and the first is the one the
  // recording itself leads with. Nothing is drawn from here -- the next
  // frame the stage shows draws it, which is a frame away.
  if (!still && tracks.length && prefs.get("subsOn")) {
    subsId = tracks[0].id;
    sel.value = String(subsId);
  }
}

/// What to call one track in the list.
///
/// Its number in the list, then the language where the recording gives one,
/// then what kind of subtitle it is. The number goes in front, the way a
/// row's number does in the clip list: it is the track's place in the list
/// and not a count of anything about the track, and read after the name it
/// looked like one -- `字幕 1` beside `文字スーパー 2` reads as a first
/// subtitle and a second crawl.
///
/// Only where there is more than one to choose between. A recording carrying
/// one is a recording with nothing to tell apart, and a lone `1` in front of
/// it is a column heading for a column of one.
function subsLabel(track, all) {
  const kind = tr(`subs.kind.${track.kind}`);
  const lang = track.language ? lang3(track.language) : null;
  const number = all.length > 1 ? `${all.indexOf(track) + 1} ` : "";
  return lang ? `${number}${lang} (${kind})` : `${number}${kind}`;
}

/// A three letter language code as something to read, where it is one of the
/// handful a recording here actually carries. Anything else is shown as it
/// arrived -- a code nobody translated is still a name, and a made-up one
/// would not be.
function lang3(code) {
  const known = tr(`lang.${code.toLowerCase()}`);
  return known.startsWith("lang.") ? code : known;
}

/// Where the picture actually is inside the stage.
///
/// `object-fit: contain` centres the picture and leaves black at two of the
/// edges; everything a subtitle is placed against is measured on the
/// picture, so the layer has to be put exactly there.
function pictureBox() {
  const img = el("preview");
  const w = img.clientWidth;
  const h = img.clientHeight;
  const nw = img.naturalWidth;
  const nh = img.naturalHeight;
  if (!w || !h || !nw || !nh) return null;
  const scale = Math.min(w / nw, h / nh);
  const pw = nw * scale;
  const ph = nh * scale;
  return { left: img.offsetLeft + (w - pw) / 2, top: img.offsetTop + (h - ph) / 2, width: pw, height: ph };
}

function clearSubs() {
  subsShown = null;
  const layer = el("subs-layer");
  if (layer) layer.hidden = true;
}

/// Ask what is on screen at `t` and draw it.
async function showSubs(t) {
  if (subsId === null || !src) {
    clearSubs();
    return;
  }
  if (subsBusy) {
    subsWanted = t;
    return;
  }
  subsBusy = true;
  const token = ++subsToken;
  try {
    const shown = await invoke("subtitle_at", { id: subsId, time: t });
    if (token !== subsToken) return;
    subsShown = shown;
    drawSubs();
  } catch (e) {
    if (token !== subsToken) return;
    // Said once, and the picker goes back to 表示しない: a recording whose
    // subtitles cannot be read is not one to say so about on every frame.
    el("status").textContent = tr("subs.failed", { e });
    subsId = null;
    subsWanted = null;
    el("subs-track").value = "";
    clearSubs();
  } finally {
    subsBusy = false;
    const next = subsWanted;
    subsWanted = null;
    if (next !== null && subsId !== null) showSubs(next);
  }
}

/// Where a Japanese font's baseline sits in the square its characters fill,
/// as a fraction of the character's height.
///
/// The square is what a caption's character field is, and what the box behind
/// a line is drawn around -- so the characters have to be placed by it. Not
/// by `textBaseline: "top"`, which was the first answer here and put the
/// *font's ascent* on that line instead: an ascent stands well above the
/// square on a Japanese font, and every line hung below its own box by a
/// third of a character. A number rather than a metric, because it is the
/// same number in every font this asks for -- 0.88 above the baseline and
/// 0.12 below is how a Japanese font divides its em -- and because the two
/// webviews the app runs on need not agree about anything to draw the same
/// picture.
const BASELINE = 0.88;

/// Draw the dots of a character the broadcaster sent the picture of.
///
/// ARIB calls them DRCS: the arrow that carries a sentence into the next
/// line and the brackets a speaker's name sits in are sent as dots rather
/// than as codes, because no character stands for them -- so there is no
/// font to ask, and what a receiver draws is the picture itself. It arrives
/// as one bit a dot and is drawn in the character cell, at whatever size
/// the stage is.
///
/// On its own little canvas first, then scaled into place: that is one
/// `drawImage`, which the browser smooths, rather than a rectangle per dot
/// at a size where a dot is less than a pixel.
function drawGlyph(ctx, glyph, colour, x, y, width, height) {
  const bits = atob(glyph.ink);
  const stride = Math.ceil(glyph.width / 8);
  const off = document.createElement("canvas");
  off.width = glyph.width;
  off.height = glyph.height;
  const octx = off.getContext("2d");
  const image = octx.createImageData(glyph.width, glyph.height);
  const r = parseInt(colour.slice(1, 3), 16);
  const g = parseInt(colour.slice(3, 5), 16);
  const b = parseInt(colour.slice(5, 7), 16);
  for (let row = 0; row < glyph.height; row++) {
    for (let col = 0; col < glyph.width; col++) {
      const byte = bits.charCodeAt(row * stride + (col >> 3)) || 0;
      if (!((byte >> (7 - (col & 7))) & 1)) continue;
      const at = (row * glyph.width + col) * 4;
      image.data[at] = r;
      image.data[at + 1] = g;
      image.data[at + 2] = b;
      image.data[at + 3] = 255;
    }
  }
  octx.putImageData(image, 0, 0);
  ctx.drawImage(off, x, y, width, height);
}

/// Put what was last read on screen, at whatever size the stage is now.
function drawSubs() {
  const layer = el("subs-layer");
  const pic = el("subs-pic");
  const canvas = el("subs-text");
  if (!layer) return;
  const box = pictureBox();
  if (!subsShown || !box) {
    layer.hidden = true;
    return;
  }
  layer.hidden = false;
  layer.style.left = `${box.left}px`;
  layer.style.top = `${box.top}px`;
  layer.style.width = `${box.width}px`;
  layer.style.height = `${box.height}px`;
  const sx = box.width / subsShown.width;
  const sy = box.height / subsShown.height;

  // A disc's subtitle is a picture of its own rectangle of the screen.
  if (subsShown.picture) {
    const p = subsShown.picture;
    pic.src = p.url;
    pic.style.left = `${p.x * sx}px`;
    pic.style.top = `${p.y * sy}px`;
    pic.style.width = `${p.width * sx}px`;
    pic.style.height = `${p.height * sy}px`;
    pic.hidden = false;
  } else {
    pic.hidden = true;
    pic.removeAttribute("src");
  }

  // And a broadcast's is characters, drawn here. On a canvas rather than in
  // elements because what a caption needs is a glyph in a given box: the
  // characters are placed one at a time, at the spacing the broadcaster
  // asked for, so nothing depends on a font's own metrics -- and the outline
  // that keeps white text readable over a white shirt is one call.
  const dpr = window.devicePixelRatio || 1;
  canvas.width = Math.round(box.width * dpr);
  canvas.height = Math.round(box.height * dpr);
  const ctx = canvas.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, box.width, box.height);
  const laid = (subsShown.runs || []).map((run) => ({
    run,
    size: run.height * sy,
    advance: run.advance * sx,
    x: run.x * sx,
    y: run.y * sy,
  }));

  // The box behind the line, which is what a receiver draws and what makes
  // white characters readable over a bright picture. The broadcaster names a
  // colour for it as well; this is not that colour, and says so in
  // `subs.rs`.
  //
  // Every box first and every character afterwards. A run ends wherever the
  // colour or the width changes -- which on a caption naming a speaker is
  // after the first bracket -- and a box drawn run by run went over the
  // characters beside it.
  const boxes = [];
  for (const { run, size, advance, x, y } of laid) {
    const rect = {
      x: x - advance * 0.08,
      y: y - size * 0.12,
      width: run.width * sx + advance * 0.16,
      height: size * 1.24,
    };
    // One box to a line rather than one to a run, so that the overlap
    // between two of them is not a darker stripe down the middle of a word.
    const last = boxes[boxes.length - 1];
    const joins =
      last &&
      Math.abs(last.y - rect.y) < 0.5 &&
      Math.abs(last.height - rect.height) < 0.5 &&
      rect.x <= last.x + last.width + 0.5;
    if (joins) last.width = Math.max(last.width, rect.x + rect.width - last.x);
    else boxes.push(rect);
  }
  ctx.fillStyle = "rgba(0,0,0,.55)";
  for (const b of boxes) ctx.fillRect(b.x, b.y, b.width, b.height);

  ctx.textBaseline = "alphabetic";
  ctx.lineJoin = "round";
  ctx.strokeStyle = "rgba(0,0,0,.9)";
  for (const { run, size, advance, x, y } of laid) {
    ctx.font = `${size}px "Noto Sans CJK JP", "Noto Sans JP", "Yu Gothic", "Hiragino Sans", "MS Gothic", sans-serif`;
    ctx.lineWidth = Math.max(1, size / 10);
    ctx.fillStyle = run.colour;
    // The character inside the field: what a format leaves around a
    // character is left around it, which is a tenth of the field on the
    // broadcasts measured here, and half of that on either side.
    const cell = advance * 0.9;
    const inset = (advance - cell) / 2;
    let at = x + inset;
    // A character the broadcaster drew rather than named. It fills the cell
    // the same way a glyph from a font does, and the box behind the line is
    // what keeps it readable, so it needs no outline of its own.
    if (run.glyph) {
      drawGlyph(ctx, run.glyph, run.colour, at, y, cell, size);
      continue;
    }
    for (const ch of Array.from(run.text)) {
      // Squeezed into that cell where the font draws it wider. The half
      // width sizes -- MSZ, and the alphanumeric set a caption switches into
      // -- put a glyph in half a field, and a font that has never heard of
      // ARIB draws a full width character at its own width whatever field it
      // was asked for: the bracket a speaker's name opens with was drawn
      // over the name beside it and lost. Narrowed only, never stretched.
      const drawn = ctx.measureText(ch).width;
      const squeeze = drawn > cell ? cell / drawn : 1;
      ctx.save();
      ctx.translate(at, y);
      ctx.scale(squeeze, 1);
      ctx.strokeText(ch, 0, BASELINE * size);
      ctx.fillText(ch, 0, BASELINE * size);
      ctx.restore();
      at += advance;
    }
  }
}

// The picture is what the layer is measured against, and a picture that
// has not arrived yet has no size: the first frame of a recording, and any
// frame whose shape differs from the one before it, are placed when the
// browser has the image rather than when it was asked for.
el("preview").addEventListener("load", () => drawSubs());

const subsPicker = el("subs-track");
if (subsPicker) {
  subsPicker.addEventListener("change", () => {
    subsId = subsPicker.value === "" ? null : Number(subsPicker.value);
    if (subsId === null) {
      clearSubs();
    } else {
      showSubs(shownTime >= 0 ? shownTime : playhead);
    }
  });
}

const seekOut = (o) => showFrame(outToSrc(clamp(o, 0, outDur)));

function updateReadouts() {
  const o = playOut();
  el("ovl-frame").textContent = String(frameNo(o));
  el("ovl-time").textContent = fmt(o);
  el("counter").textContent = tr("editor.counter", {
    at: frameNo(o),
    all: outFrames(),
    t: fmt(o),
  });
  // OUT is part of the selection, so its own picture counts towards the length
  const sel = tr("editor.selection", {
    a: frameNo(selA),
    b: frameNo(selB),
    len: fmt(selEnd() - selA),
  });
  el("selection").textContent = sel;
  el("ovl-sel").textContent = sel;
}

// --- the readouts on the picture ------------------------------------------
//
// Drawn unless they are turned off, from the info bar. They stand at the foot
// of the picture, which is where a subtitle stands too, and a stage has no
// third place to put either of them: the readouts are drawn over the
// subtitles so that neither can go missing, and which of the two may be in
// the way is a question for the person cutting rather than for this window.
//
// The line under the film strip says the same thing and is not touched --
// what this hides is the box on the picture, which is the one that is in the
// way of anything.

/// Kept with the rest of 環境設定, which is where it can also be answered from
/// -- the editor window is built afresh for every clip, and an answer given
/// once about what the picture carries should not have to be given again.
/// The button on the info bar and the box in the panel write the same thing.
const counterWanted = () => !!prefs.get("counter");

/// Put them up or take them down, and leave the button showing which it is.
/// `remember` is false for the window doing as it was already told, and true
/// for the person telling it.
function showCounter(on, remember = true) {
  el("overlay").hidden = !on;
  const button = el("counter-show");
  if (button) {
    button.classList.toggle("on", on);
    button.setAttribute("aria-pressed", on ? "true" : "false");
  }
  if (!remember) return;
  prefs.set("counter", on);
}

const counterButton = el("counter-show");
if (counterButton) {
  showCounter(counterWanted(), false);
  // Read back off the picture rather than off a flag of its own: the one on
  // the stage is what the button is about, and two of them would be one too
  // many things to keep in step.
  counterButton.addEventListener("click", () => showCounter(el("overlay").hidden));
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

/// The cells a GOP-divided reel is made of: `slots` of them, each beginning
/// on a GOP boundary and covering about `span / vis` of the recording,
/// centred on the GOP the playhead stands in. `at` is the picture to show and
/// the time to caption; `a` and `b` are the stretch the cell speaks for,
/// which is what the playhead is placed against.
///
/// A cell is as long as the menu asked for, rounded to the nearest boundary
/// either side. That is what "GOP・3 分" means once the widths are fixed:
/// three minutes across the window, near enough, with every cell still
/// standing on a place a cut is free. At the short end a cell cannot hold
/// less than one GOP, so the window covers rather more than it says and every
/// boundary is drawn -- the honest answer, and the one that reads.
///
/// **Chosen by time, not by counting boundaries.** Giving each cell a fixed
/// number of GOPs is the same thing only where the GOPs are evenly spaced,
/// which broadcast material is and a disc is not: a Blu-ray puts an entry
/// point at every scene change as well as every second or so, and on one
/// VC-1 disc they run from 0.067 s to 0.801 s apart. One cell per GOP drew
/// those as cells of equal width standing for stretches of time twelve times
/// apart -- a window of nine cells covering 0.6 s in an action scene and
/// 7.2 s in a quiet one, both labelled "6 秒". The strip stopped being a
/// ruler: the playhead crawled across a cell and then jumped four of them,
/// and clicking a place on it landed nowhere near where it looked.
///
/// Slots that fall outside the recording are kept, as blanks. They are what
/// lets the reel slide far enough to hold the playhead at the middle when it
/// is near either end; without them the reel would run out and the marker
/// would drift off the picture it is meant to be standing on.
function gopCells(o, span, slots, vis) {
  const n = gops.length;
  // A recording with nothing in it to divide on. The reel is cut on an even
  // grid instead -- the same cells at the same widths, standing for stretches
  // of time rather than for runs of GOPs. See `refreshStrip`, which sends
  // every unwalked recording the same way.
  if (!n) return evenCells(o, span, slots, vis);
  // the GOP the playhead is standing in
  let i0 = 0;
  while (i0 + 1 < n && gops[i0 + 1] <= o + 1e-9) i0++;
  // What one cell is meant to cover.
  const d = Math.max(span / Math.max(vis, 1), 1e-3);

  // The boundary each cell begins on: the playhead's, then the one nearest
  // `d` further on, and so outwards in both directions. Nearest rather than
  // the first one past it, which would round every cell up and hand a
  // recording with 0.5 s GOPs a window half as wide again as the menu says.
  // `-1` where the recording has run out, which the loops below leave blank.
  const half = slots >> 1;
  const marks = new Array(slots).fill(-1);
  marks[half] = i0;
  for (let k = half + 1, j = i0; k < slots; k++) {
    const want = gops[j] + d;
    let m = j + 1;
    while (m < n && gops[m] < want - 1e-9) m++;
    if (m >= n) break;
    // The boundary before it is nearer as often as not -- but never the
    // cell's own, which would give it no width at all, and never one that
    // would leave the cell less than half the width it was asked for. Where
    // the boundaries are dense and then stop, nearest on its own picks the
    // last of the dense run and draws a sliver beside a full-width cell,
    // which is the unevenness this is here to stop.
    if (m - 1 > j && gops[m - 1] - gops[j] >= d / 2 && want - gops[m - 1] < gops[m] - want) {
      m--;
    }
    marks[k] = m;
    j = m;
  }
  for (let k = half - 1, j = i0; k >= 0; k--) {
    const want = gops[j] - d;
    let m = j - 1;
    while (m >= 0 && gops[m] > want + 1e-9) m--;
    if (m < 0) break;
    if (m + 1 < j && gops[j] - gops[m + 1] >= d / 2 && gops[m + 1] - want < want - gops[m]) {
      m++;
    }
    marks[k] = m;
    j = m;
  }

  const cells = [];
  for (let k = 0; k < slots; k++) {
    const j = marks[k];
    if (j < 0) {
      cells.push({ live: false });
      continue;
    }
    const a = gops[j];
    // A cell runs to where the next one begins, so that the reel tiles the
    // recording without a gap or an overlap. The outermost one has no next
    // cell to end at and takes the width it was asked for -- not the rest of
    // the recording, which would make the reel's last cell stand for half an
    // hour and drag the playhead across it at a crawl.
    const next = marks[k + 1];
    const b = Math.min(next >= 0 ? gops[next] : a + d, outDur);
    cells.push({ at: a, a, b: Math.max(b, a + 1e-3), live: true });
  }
  // A blank has no time of its own, so it takes over where the cell beside it
  // leaves off. The reel stays continuous in time that way, and the playhead
  // can be found on it whichever slot it happens to fall in.
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

/// The reel cut on an even grid, for a recording whose GOP boundaries are not
/// known yet. Same shape of answer as [`gopCells`]: `slots` cells centred on
/// the one the playhead stands in, blanks past either end.
function evenCells(o, span, slots, vis) {
  const step = Math.max(span / Math.max(vis, 1), 1e-3);
  const i0 = Math.floor(o / step);
  const half = slots >> 1;
  const cells = [];
  for (let k = -half; k < slots - half; k++) {
    const a = (i0 + k) * step;
    const b = a + step;
    // Past either end of the material: kept as blanks so the reel can still
    // slide far enough to hold the playhead in the middle.
    cells.push(a < -1e-9 || a >= outDur ? { a, b, live: false } : { at: a, a, b, live: true });
  }
  return cells;
}

/// Pictures found before the walk, keyed by their own instant in milliseconds.
///
/// By the picture's instant rather than by the instant asked for, because the
/// two are not the same and because two ways of asking share this: a seek per
/// cell (`glimpses`) and a read through the whole reel (`glimpse_sweep`). What
/// a cell wants to know is "is there a picture from inside me", and that is a
/// question about where the pictures are.
const glances = new Map();
/// What has already been asked for, so that a strip redrawn where it stood
/// does not pay for it twice: `a<ms>` for a cell's own seek, `s<ms>-<ms>` for
/// a stretch read through. The grid the cells are cut on before the walk
/// stands on the recording's own clock rather than on the playhead, so
/// scrubbing back over a stretch asks for the very same things again.
const asked = new Set();
/// Which spans have been found to want the reel read through rather than
/// sought cell by cell -- `read` -- and which have been found not to, `no
/// read`. A span nothing is known about yet is missing from this, and gets the
/// seeks first and the reading after if they left gaps.
///
/// Per span because it is the *cell's* width against the recording's own
/// spacing that decides it, and the menu is what sets the cell width.
const ways = new Map();
/// How many pictures to keep. They are 200px JPEGs, so a few hundred is a
/// megabyte or two, and the walk is over long before that fills.
const GLANCE_KEEP = 400;
/// The most of the recording worth reading through to fill one reel. Beyond
/// this the read costs more than the gaps are worth: measured on broadcast
/// material, a reel covering 6s reads through in a third of a second and one
/// covering 30s in a second and a half. See `examples/glancecost.rs`.
const SWEEP_MAX = 8;

/// Is the playhead being moved right now?
///
/// A search on the strip, a drag on the scrubber, a run of wheel notches or a
/// held step button, or playback. **While it is, the strip is filled the cheap
/// way only.** Reading a reel through takes a third of a second on broadcast
/// material and two on a disc, which is an age when the strip is meant to be
/// following the hand -- and the reel it was read for is gone by the time it
/// arrives. The seeks are a tenth of that and they keep up.
///
/// Every one of these ends by asking for the picture it stopped on, and that
/// asks for the strip again (`showFrame` -> `scheduleStrip`), so the reading
/// happens as soon as the hand comes off.
const moving = () => !!search || !!dragging || scrubBusy || playing;

function forgetGlances() {
  glances.clear();
  asked.clear();
  ways.clear();
}

/// Draw the reel, and fill it with what can be found without the walk.
///
/// Two ways of finding a picture. Both are approximate in the same way -- with
/// no access points to seek by, the container's own seek lands on the entry
/// point at or before the instant asked for, a GOP out -- so **a picture goes
/// under the cell it fell in, not under the cell that asked for it**, and the
/// caption and the click follow the picture rather than the instant. The cell
/// keeps its place and its width: the strip is a ruler, and a ruler with
/// uneven marks is worse than a blank one.
///
/// **A seek per cell** (`glimpses`) is the cheap way and is all a wide reel
/// needs. What it cannot do is answer a cell narrower than the recording's
/// GOP, or two cells lying between the same pair of entry points: one ask, one
/// picture, and the other cell stays black. That is where the gaps in a
/// stage-one strip come from, and it is a question of the cell's width against
/// the recording's own spacing -- half a second between entry points on most
/// broadcast material, a whole second on some stations, against cells 0.55s
/// wide at the default setting and 0.27s at the closest.
///
/// **A read through the reel** (`glimpse_sweep`) decodes the stretch and keeps
/// the first picture in each cell, entry point or not, so every cell of it is
/// answered. It costs in proportion to the stretch rather than to the number
/// of cells, so it is only worth it on a short reel, and it is never done
/// while the hand is on the playhead. See [`moving`].
///
/// Which way a span gets is settled by trying them: the seeks first, and the
/// reading after if they left gaps. A span the reading fills better than the
/// seeks did goes straight to the reading from then on; one it does not is
/// left to the seeks for good.
async function fillByGlance(shots, cells, unit, win, span) {
  if (!src) {
    renderStrip(shots, unit, win);
    return;
  }
  const way = ways.get(span);
  const idx = cells.map((c, i) => (c.live ? i : -1)).filter((i) => i >= 0);
  const key = (t) => Math.round(t * 1000);
  const keep = (g) => g && glances.set(key(g.time), { url: g.url, time: g.time });

  /// Put the pictures now in hand onto the reel, and say how many cells that
  /// filled. Run after every answer, and once before the first, so that a
  /// redraw blanks only the cells it has never had a picture for.
  ///
  /// Earliest first, so that a cell holding two of them shows the one its
  /// stretch begins with -- which is what the strip shows once the walk has
  /// landed, and keeps the two from disagreeing.
  const place = () => {
    let filled = 0;
    const taken = new Array(cells.length).fill(false);
    const held = [...glances.values()].sort((a, b) => a.time - b.time);
    for (const g of held) {
      const at = srcToOut(g.time);
      // A picture out of material the cuts took away belongs to nobody, and
      // one from outside the drawn reel has no cell to go in.
      if (at === null) continue;
      const j = cells.findIndex((c) => c.live && at >= c.a - 1e-9 && at < c.b);
      if (j < 0 || taken[j]) continue;
      taken[j] = true;
      shots[j].url = g.url;
      shots[j].time = g.time;
      shots[j].at = at;
      filled++;
    }
    return filled;
  };

  const room = () => {
    // Whatever is held is worth more than what is about to be asked for --
    // the reel on screen is drawn from it -- but a session that scrubbed a
    // recording end to end would fill memory with pictures it will never show
    // again. Emptied wholesale rather than by age: the asks go with them, and
    // pairing the two up is more bookkeeping than a rebuild costs.
    if (glances.size <= GLANCE_KEEP) return;
    glances.clear();
    asked.clear();
  };

  let filled = place();
  renderStrip(shots, unit, win);

  const quiet = !moving();
  // The cheap way. Skipped only where the reel has already been found to want
  // the reading *and* there is time to do it: with a hand on the playhead the
  // reading is off, and then these are the only pictures there are.
  if (way !== "read" || !quiet) {
    // The middle of each cell rather than its edge. A landing lands at or
    // before its ask, so asking on the edges would put half of them in the
    // cell before the reel begins.
    const times = idx.map((i) => outToSrc((cells[i].a + cells[i].b) / 2));
    const want = times.filter((t) => !asked.has(`a${key(t)}`));
    if (want.length) {
      const token = ++stripToken;
      let got;
      try {
        got = await invoke("glimpses", { path: src.path, times: want, width: 200 });
      } catch (e) {
        jlog(`glimpses: ${e}`);
        return;
      }
      if (token !== stripToken || walked()) return;
      room();
      want.forEach((t, i) => {
        asked.add(`a${key(t)}`);
        keep(got[i]);
      });
      filled = place();
      renderStrip(shots, unit, win);
    }
  }

  // The thorough way, on a reel short enough to read through and only while
  // nothing is moving. The stretch is the reel's own, in source time: a reel
  // that walks over a cut covers two pieces of the recording and everything
  // between them, which is why the length is measured here rather than taken
  // from the menu.
  const from = outToSrc(cells[idx[0]].a);
  const to = outToSrc(cells[idx[idx.length - 1]].b);
  const roomy = to - from > 0 && to - from <= SWEEP_MAX;
  // Nothing to gain where every cell already has a picture, whichever way
  // this span is usually filled.
  if (way === "no read" || !roomy || !quiet || filled >= idx.length) return;
  const k = `s${key(from)}-${key(to)}`;
  if (asked.has(k)) return;
  const token = ++stripToken;
  let got;
  try {
    got = await invoke("glimpse_sweep", {
      path: src.path,
      from,
      to,
      width: 200,
      // How the reel is divided, so that a stretch carrying entry points ten
      // to the cell is not encoded ten times over. Nothing to divide on where
      // a cut falls inside the reel: the cells tile the *edited* timeline, and
      // this stretch is a run of the recording.
      cell: outRangeToSrc(cells[idx[0]].a, cells[idx[idx.length - 1]].b).length > 1 ? 0 : unit,
      // Room well above one per cell: the thinning above is what keeps the
      // count down, and this is only a ceiling.
      most: idx.length * 3,
    });
  } catch (e) {
    jlog(`glimpse_sweep: ${e}`);
    return;
  }
  // A walk that landed while this was in flight has rebased the output clock --
  // the timeline begins at the first access point now -- so these instants no
  // longer fall in the cells they were asked for. The reel is about to be drawn
  // from the access points themselves; leave it to that.
  if (token !== stripToken || walked()) return;
  room();
  asked.add(k);
  for (const g of got) keep(g);
  // Drawn again whatever the count says. A cell can only gain a picture here,
  // and the count is of the cells *this* pass filled: where two pictures fall
  // in one cell only one of them counts, so it can come back lower than the
  // reel is actually showing.
  place();
  renderStrip(shots, unit, win);
  // Whether this span wants the reading from now on -- which is to say whether
  // the seeks can be skipped, the reading finding everything they do and more.
  //
  // **Not "did it fill more cells than the seeks did".** That was tried, and it
  // locks the wrong spans out. The reel drawn as the editor opens sits at the
  // head of the recording, where the cells before the first entry point cannot
  // be filled by anything at all, so on a recording whose seeks already answer
  // nine cells in ten the reading has nothing to add on that one reel -- and
  // two of five broadcast recordings settled "no read" there and spent the rest
  // of the walk with a gap in every reel, which is the whole of what this is
  // here to close. What says the reading is not worth repeating is the reading
  // coming back with nothing at all.
  if (way === undefined) ways.set(span, got.length ? "read" : "no read");
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
  // Before the walk there are no access points to divide the reel on, so it
  // is cut on an even grid instead. Asked here rather than inside `gopCells`,
  // which cannot tell the difference: `gops` carries the start of every
  // surviving segment whether the walk has been over the recording or not, so
  // an unwalked recording arrives there looking like one with a single
  // boundary at zero -- and a reel cut on that is one cell wide.
  const cells = walked()
    ? gopCells(o, view.span, slots, vis)
    : evenCells(o, view.span, slots, vis);
  const live = cells.filter((c) => c.live);
  if (!live.length) return;

  // Before the walk there are no held pictures and no opened recording to
  // decode an exact one out of. What there is is the container's own seek,
  // which is what the stage has been drawing with all along -- so the reel is
  // drawn as it stands, and filled with what that seek can reach. See
  // `fillByGlance`.
  if (!walked()) {
    const shots = cells.map((c) => ({
      url: null,
      time: null,
      at: c.live ? c.at : c.a,
      a: c.a,
      b: c.b,
      px,
    }));
    const mid = live[live.length >> 1];
    const unit = mid.b - mid.a;
    const win = { vis: vis * unit, rest: o, byNearest: false };
    await fillByGlance(shots, cells, unit, win, view.span);
    return;
  }
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
let lastSharp = 0;

/// How far behind the stage has to be before a stand-in is worth putting on
/// it. A quarter of a second: less than that and the picture up is close
/// enough that covering it costs more than the wait it saves.
const STANDIN_STALE = 0.25;

/// A held key picture on the stage, as a stand-in until a real one arrives.
///
/// The held pictures are 192px wide and sit on key pictures -- 0.6s apart on
/// the broadcast material this is for -- so a stand-in is both soft and up to
/// nine frames from the time asked for. What it buys is arriving now instead
/// of after a decode, and that is worth having while a drag or a search
/// crosses ground faster than anything can be decoded. Stepping a frame at a
/// time it is worth nothing: the picture already up is one frame from the
/// answer and the real one is milliseconds behind it. Laying a soft key
/// picture over that and taking it back is what made stepping flicker, on the
/// frames where the key picture was far enough away to look like a different
/// shot.
///
/// So the stage has to be behind before a stand-in is fetched at all, and the
/// stand-in has to be nearer the mark than the picture it would cover. Both
/// are measured against what is actually on the stage rather than against
/// `interval`, which is only the floor under the spacing -- the pass keeps
/// every key picture it is offered, so the pictures land where the recording
/// puts them and the floor can sit an order of magnitude below that.
async function paintFast(t) {
  // The caption belongs to the instant rather than to the picture, so it is
  // asked for here as well as in `showFrame` -- this is the path a drag, a
  // wheel and the right-click search move the playhead along, and without it
  // the line on the stage was the one from wherever the drag started.
  showSubs(t);
  if (!held) return;
  if (shownTime >= 0 && Math.abs(t - shownTime) < STANDIN_STALE) return;
  const token = ++hoverToken;
  const shot = await invoke("hover_thumb", { time: t });
  if (token !== hoverToken || !shot) return;
  if (shownTime >= 0 && Math.abs(t - shot.time) >= Math.abs(t - shownTime)) return;
  shownTime = shot.time;
  el("preview").src = shot.url;
  el("ovl-kind").textContent = tr("editor.searchKind");
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
    if (token !== previewToken) return;
    el("preview").src = shot.url;
    shownTime = shot.time;
  } catch {
    /* the next tick will try again */
  }
}

/// How often a scroll moves the playhead, in milliseconds. Fourteen times a
/// second: fast enough to look like movement, slow enough that the held
/// pictures behind it keep up.
const SCROLL_TICK = 70;

/// The fastest this program scrolls: sixty times the recording's own speed,
/// which crosses a half-hour programme in half a minute. The strip's right
/// drag reaches it at the far edge, and a page key whose unit is a percent
/// asks for a share of it.
///
/// A share of *this* rather than a share of the timeline. A share of the
/// timeline sounds like the same kind of answer and is not: a quarter of an
/// hour's recording a second is nine hundred times speed, so every number
/// somebody could type would be too fast to read, and the same number would
/// mean something else on the next recording. A multiple of the recording's
/// own speed is a speed somebody can picture and keep.
const SCROLL_MAX_RATE = 60;

/// Run a scroll until `endScroll` stops it.
///
/// Two things start one. The strip's right drag, whose speed is where the
/// pointer is, and a held page key whose speed is a preference. The loop is
/// the same either way, so the speed is not a number but something asked at
/// every tick: the pointer moves under the drag, and under the key both the
/// preference and the modifiers held with it can change while the key is
/// down.
function startScroll(rateAt, extra) {
  if (!src) return;
  // One at a time. A drag begun over a running 早送り would otherwise leave
  // that one's timer going with nothing holding its handle, and two of them
  // move the playhead twice per tick.
  if (search) clearInterval(search.timer);
  search = { rateAt, ...extra };
  el("searching").hidden = false;
  const tick = () => {
    const rate = search.rateAt();
    if (!rate) return;
    playhead = outToSrc(
      clamp(playOut() + rate * (SCROLL_TICK / 1000), 0, Math.max(0, outDur - frame())),
    );
    updateReadouts();
    draw();
    paintFast(playhead);
    askStrip();
    if (Math.abs(rate) < 2.5 && Date.now() - lastSharp > 320) {
      lastSharp = Date.now();
      paintSharp(playhead);
    }
  };
  search.timer = setInterval(tick, SCROLL_TICK);
  // The first step now rather than a tick from now, or a page key tapped
  // rather than held would move nothing at all.
  tick();
}

function startSearch(ev) {
  // A drag begun over a running 早送り takes the scroll over, so that button
  // is no longer running anything and must stop saying it is. `endScroll`
  // is what puts it down; without a picture, because the drag is about to
  // ask for one of its own.
  if (seekRate) endScroll(false);
  const rect = el("strip").getBoundingClientRect();
  const half = rect.width / 2;
  // cubed, so the middle of the strip is a fine crawl and the far edges
  // cross a half-hour recording in half a minute
  const rate = () => {
    const dx = clamp((search.x - (rect.left + half)) / half, -1, 1);
    return Math.sign(dx) * Math.abs(dx) ** 3 * SCROLL_MAX_RATE;
  };
  startScroll(rate, { x: ev.clientX });
}

/// The scroll a held PageUp or PageDown does, where that key's preference is
/// a speed rather than an amount.
///
/// `dir` is which way, and the speed comes out of `pageStep` at every tick
/// rather than once here. That is what lets Ctrl pressed halfway through a
/// hold change how fast the recording is going past, and a number typed in
/// the other window take effect under the held key: the keyboard repeats the
/// key while it is down, and each repeat is a fresh answer from the same
/// preferences.
function startPageScroll(dir, ev) {
  if (search && search.page) {
    search.dir = dir;
    search.press = ev;
    return;
  }
  endScroll();
  startScroll(() => pageStep(search.press).rate * search.dir, {
    page: true,
    dir,
    press: ev,
  });
}

/// Stop whatever is scrolling: the strip's right drag, a held page key, or
/// the 早送り / 巻き戻し buttons. `repaint` is false where a picture of where
/// it stopped is about to be overtaken anyway -- 再生 starting from here.
function endScroll(repaint = true) {
  // The buttons have to stop showing a speed whichever of the several ends
  // it was, this one included: a drag started over a running 早送り takes
  // the scroll over, and the mouse going up ends it here.
  if (seekRate) {
    seekRate = 0;
    paintSeek();
  }
  if (!search) return;
  clearInterval(search.timer);
  search = null;
  el("searching").hidden = true;
  if (repaint) showFrame(playhead);
}

el("strip").addEventListener("mousedown", (ev) => {
  if (ev.button !== 2) return;
  ev.preventDefault();
  startSearch(ev);
});
window.addEventListener("mousemove", (ev) => {
  if (search) search.x = ev.clientX;
});
window.addEventListener("mouseup", (ev) => {
  if (ev.button === 2) endScroll();
});
// A key that comes up stops the scroll it started. Either page key does:
// which of the pair is released first is not worth telling apart, and a
// window that loses the keyboard never hears the release at all.
window.addEventListener("keyup", (ev) => {
  if (ev.key === "PageUp" || ev.key === "PageDown") {
    if (search && search.page) endScroll();
  }
});
window.addEventListener("blur", () => {
  if (search && search.page) endScroll();
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
  // A hand on anything that moves the playhead puts a running 早送り down:
  // two things moving it at once is neither of them.
  if (seekRate) endScroll();
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

/// Whether a wheel over this element belongs to something that scrolls.
///
/// The marks down the left and the plan's segment list are boxes with more in
/// them than fits, and a notch over one of those is meant for it. Everywhere
/// else the wheel moves the playhead -- which used to be true of the film
/// strip alone, and is now true of the window, the way the reference tool
/// has it. Somebody reaching for the wheel over the picture is reaching for
/// the same thing they reach for over the strip.
function scrolls(node) {
  for (let n = node; n && n !== document.body; n = n.parentElement) {
    if (n.scrollHeight - n.clientHeight > 1) {
      const how = getComputedStyle(n).overflowY;
      if (how === "auto" || how === "scroll") return true;
    }
  }
  return false;
}

window.addEventListener(
  "wheel",
  (ev) => {
    // The wheel over the volume is the volume. A pointer standing on a
    // control is asking about that control, and everywhere else in this
    // window the wheel moves the playhead -- which, with the pointer down
    // here, is nowhere the eye is.
    if (ev.target.closest?.(".vol")) {
      ev.preventDefault();
      if (!el("volume").disabled) {
        takeVolume(Number(el("volume").value) - Math.sign(ev.deltaY) * 5);
      }
      return;
    }
    if (!src) return;
    // A panel over the timeline is the program not listening to the timeline;
    // the same reason the keys stop at one. See the keydown handler.
    if (!el("tracks-modal").hidden) return;
    if (scrolls(ev.target)) return;
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

/// Which run of playback this is, counting from the window's own side.
///
/// A stop and the next start can land inside one turn of this loop -- ◀◀ and
/// ▶▶ are exactly that -- and the engine cannot tell the two apart by a flag
/// alone: see `Playing` on the other side. Each 再生 is named here, the name
/// goes with the request, and the end of a run comes back carrying it.
let playRun = 0;

/// The picture on the stage while playback runs, as a URL that has to be
/// handed back. A blob URL holds its bytes until it is revoked, and thirty a
/// second is a leak with a shape.
let playUrl = null;
/// The instant of the last picture actually shown, so a picture that arrives
/// after a newer one can be dropped instead of stepping the stage backwards.
let playAt = -Infinity;

/// Give back the blob the stage is holding, if it is holding one.
function dropPlayUrl() {
  if (!playUrl) return;
  URL.revokeObjectURL(playUrl);
  playUrl = null;
}

/// One picture off the playback channel: eight bytes of instant, then the
/// JPEG. See the `play` command for why the two travel as one message.
///
/// `run` is the playback it came from. A run that has been left behind -- a
/// ◀◀ stopped it and asked for another from ten seconds back -- has pictures
/// already on their way, and they arrive after the new run has begun. Since
/// they are further along they would set `playAt` past everything the new
/// run is about to send, and every picture of it would then be dropped as
/// too old: the skip looked like playback simply stopping where it was.
function showPlayFrame(run, buf) {
  if (run !== playRun || !playing || !buf || buf.byteLength <= 8) return;
  const t = new DataView(buf).getFloat64(0, true);
  // The large payloads are fetched by this window rather than handed to it,
  // and two fetches can finish in the other order. A picture older than the
  // one on the stage is one nobody wants back.
  if (t < playAt) return;
  playAt = t;
  const was = playUrl;
  playUrl = URL.createObjectURL(new Blob([new Uint8Array(buf, 8)], { type: "image/jpeg" }));
  playhead = t;
  el("preview").src = playUrl;
  shownTime = t;
  if (was) URL.revokeObjectURL(was);
  updateReadouts();
  draw();
  showSubs(t);
  // The strip is not redrawn here -- it is already sliding, and this is
  // what it slides against.
  anchorPlay(srcToOutSeam(t));
}

function setPlaying(on) {
  playing = on;
  el("play").textContent = tr(on ? "t.stop" : "t.play");
  el("play").classList.toggle("on", on);
  if (!on) {
    playAnchor = null;
    if (reelRaf) cancelAnimationFrame(reelRaf);
    reelRaf = 0;
    // The stage is about to be given a picture of its own; the blob behind
    // the last played one is nobody's after that.
    dropPlayUrl();
  }
}

/// Whether 再生 repeats. A mode, not an action: what it changes is what the
/// next 再生 plays and what happens when that playback runs out.
let looping = false;
/// Where a looping playback goes back to, in output time, or null when
/// nothing is looping.
let loopAt = null;

/// A round is at least this long, in seconds.
///
/// A round shorter than the seek that starts it is not a loop, it is a
/// stutter -- and two pictures is a selection somebody lands on by putting
/// OUT down before moving. Short ones are stretched to this rather than
/// refused: a 再生 that quietly did nothing would be the worse answer of the
/// two, and half a second either side of what was marked is still the join
/// that was marked.
const MIN_LOOP = 0.5;

/// The stretch ループ plays, in output time, or null when there is nothing
/// to play at all.
///
/// The selection, which with nothing marked is the whole timeline -- the
/// same thing ✂ would take, listened to instead of removed. Entered at the
/// playhead when it already stands inside it, so that standing on a join and
/// pressing 再生 plays that join over and over rather than starting again
/// from the top of the recording.
function loopRange() {
  const a = selA;
  const at = playOut();
  const from = at > a + 1e-9 && at < selEnd() - frame() ? at : a;
  const b = Math.min(outDur, Math.max(selEnd(), from + MIN_LOOP));
  return b - from > 0.1 ? { from, b } : null;
}

function startPlay() {
  if (!src || playing || outDur <= 0) return;
  // 再生 out of a 早送り: the search stops where it got to, and this starts
  // from there. No picture of that instant is asked for -- the first one
  // playback sends is along in a third of a second and is the same picture.
  if (seekRate) endScroll(false);
  // With ループ on it is the selection that plays, and the engine is given
  // that stretch alone: playback ends when the ranges run out, and this is
  // what makes it run out at OUT instead of at the end of the recording.
  const round = looping ? loopRange() : null;
  if (looping && !round) return;
  loopAt = round ? round.from : null;
  const ranges = round
    ? outRangeToSrc(round.from, round.b).map((r) => [r.a, r.b])
    : outputRanges();
  const from = round ? outToSrc(round.from) : playhead;
  setPlaying(true);
  // Redraw before the first picture arrives: the reel standing there was
  // drawn without a margin, and there is nowhere for it to slide.
  clearTimeout(stripTimer);
  stripTimer = null;
  askStrip();
  reelRaf = requestAnimationFrame(reelTick);
  // The recording's own rate, so that playback moves the way the recording
  // does. Asking for less was the safe thing while nothing dropped a picture
  // that had already come due -- a machine that could not keep up ran the
  // whole preview slow, and slow against sound that plays at the card's own
  // speed is out of step. Now the pacing lets a late picture go by (see
  // `play` in the Rust side), so asking for every one of them costs nothing
  // on a machine that cannot show them all: what it cannot do it skips, and
  // what it can it shows at the right moment.
  const fps = src.fps > 0 ? src.fps : 30;
  // Capped at 1280 whatever the stage asks for. Each picture costs a scale,
  // a JPEG and a trip through the channel, so this is the one place where
  // dropping below the stage's full request buys back frame rate -- and
  // where there is a proxy, 1280 is also its own width, past which the extra
  // pixels are invented.
  const width = Math.min(stageWidth(), 1280);
  // A channel of its own for the pictures. An event would carry each one as
  // JSON, which means base64, which at this width is 4 MB/s of text a second
  // for the window to parse; a channel carries the bytes. See `play` on the
  // Rust side.
  const run = ++playRun;
  const frames = new T.core.Channel();
  frames.onmessage = (buf) => showPlayFrame(run, buf);
  playAt = -Infinity;
  // not awaited: it resolves when playback ends, and `play-ended` says so
  invoke("play", { ranges, from, width, fps, run, frames }).catch((e) => {
    el("status").textContent = tr("editor.playFailed", { e });
    setPlaying(false);
  });
}

/// Stop, and put a picture of where it stopped on the stage.
///
/// `repaint` is false for the stops that are about to be followed by another
/// 再生 -- see [`movePlayhead`], which is the other half of that.
function stopPlay(repaint = true) {
  if (!playing) return;
  invoke("stop_play");
  setPlaying(false);
  if (repaint) showFrame(playhead);
}

/// Put the playhead at `o` -- an output time -- without asking for a picture
/// of it.
///
/// For the moves that are about to be followed by playback, which is to say
/// ◀◀, ▶▶ and a loop coming round again. A still asked for at one of those
/// lands *after* the new playback has started, and `showFrame` reads a
/// picture arriving from anywhere else as the playhead having been moved by
/// hand -- so the still that was only there to fill a third of a second
/// stops the playback it was filling for. Playback's own pictures fill the
/// stage instead, and the counter and the scrubber are moved here so that
/// they do not sit on the old place while the first one is decoded.
function movePlayhead(o) {
  playhead = outToSrc(clamp(o, 0, outDur));
  updateReadouts();
  draw();
}

el("play").addEventListener("click", () => (playing ? stopPlay() : startPlay()));

// --- 早送りと巻き戻し -----------------------------------------------------
//
// The search this window already runs -- the strip's right drag and a page
// key whose unit is a percent are the same loop -- at a speed the button
// says rather than one the pointer is deciding. See `startScroll`.
//
// Not playback at a multiple of the rate. Backwards there is no such thing:
// a decoder plays one way, and going the other means decoding each GOP from
// its start to show its pictures in reverse, which at any useful speed is
// more decoding than the machine has. Forwards it would mean asking for
// pictures at four times the rate and showing one in four. What the eye
// wants out of 早送り is the recording going past, and that is what a search
// already is. Silent, for the same reason: there is nothing to hear at four
// times speed, and the sound card plays at its own rate whatever the picture
// is doing.

/// The ladder the two buttons climb, in multiples of the recording's own
/// speed. Four rungs, the top one a third of what the drag reaches at the
/// edge of the strip -- past that the pictures go by faster than they can be
/// read, and covering ground is what the drag and the page keys are for.
const SEEK_SPEEDS = [2, 4, 8, 16];

/// The speed one of these two buttons is running at, signed, or 0 for none.
let seekRate = 0;

/// Put the speed on the button that is running, and take it off the other.
function paintSeek() {
  for (const [id, dir, glyph] of [
    ["rewind", -1, "◀◀"],
    ["fast-fwd", 1, "▶▶"],
  ]) {
    const on = seekRate !== 0 && Math.sign(seekRate) === dir;
    el(id).classList.toggle("on", on);
    el(id).textContent = on ? `${glyph} ${Math.abs(seekRate)}×` : glyph;
  }
}

/// Start one, or step the one already running.
///
/// The same button again doubles the speed; the other one starts over the
/// other way. Past the top rung it stops, so the button that started it is
/// also the way out of it: this row has no 停止 of its own, and a button
/// that cycles for ever is one that cannot be put down.
function fastPlay(dir) {
  if (playing) stopPlay(false);
  const rung = Math.sign(seekRate) === dir ? SEEK_SPEEDS.indexOf(Math.abs(seekRate)) + 1 : 0;
  if (rung >= SEEK_SPEEDS.length) {
    endScroll();
    return;
  }
  seekRate = dir * SEEK_SPEEDS[rung];
  paintSeek();
  // Read at every tick rather than handed over once, so that stepping the
  // speed does not have to stop the scroll and start another.
  startScroll(() => seekRate, { seek: true });
}

el("rewind").addEventListener("click", () => fastPlay(-1));
el("fast-fwd").addEventListener("click", () => fastPlay(1));

el("loop").addEventListener("click", () => {
  looping = !looping;
  el("loop").classList.toggle("on", looping);
  el("loop").setAttribute("aria-pressed", looping ? "true" : "false");
  // Turned over while something is playing, it takes effect on what is
  // playing. Waiting for the end would mean waiting for the end of the
  // recording, which is the thing the answer has just changed.
  if (playing) {
    stopPlay();
    startPlay();
  }
});

// --- volume -------------------------------------------------------------
//
// How loud the preview is, which is nothing to do with what gets written:
// the output carries the recording's own sound whatever this says. It is a
// checking level -- a join is listened to at whatever is comfortable in the
// room the cutting is being done in, and a broadcast's own level is
// frequently not that.
//
// Kept with the rest of 環境設定, like the counter beside it and for the same
// reason: this window is built afresh for every clip, and somebody who works
// at a third of the way up should not have to say so at each one.

/// What the slider's position comes to as a multiplier on the samples.
///
/// The square of it rather than the position itself. Loudness is not linear
/// in amplitude -- half the amplitude is nothing like half as loud -- so a
/// slider handed straight through spends its top half doing almost nothing
/// and then drops away all at once near the bottom. Squaring puts the useful
/// part of the range under the middle of the travel, which is where the hand
/// is.
const gain = (at) => (at / 100) ** 2;

/// Whether the sound is silenced. The ♪ button is the state; a flag of its
/// own would be one more thing to keep in step, the same reason the counter
/// is read back off the picture.
const muted = () => el("mute").classList.contains("muted");

/// Settle the level, tell the engine, and leave the two controls showing it.
/// `remember` is false for the window doing as it was already told, and true
/// for the person telling it.
function showVolume(at, silent, remember = true) {
  at = clamp(Math.round(Number(at)), 0, 100);
  el("volume").value = String(at);
  // How far along the bar is filled. The stylesheet draws it; this is the
  // one number it needs, and a slider cannot say it for itself.
  el("volume").style.setProperty("--at", `${at}%`);
  el("vol-num").textContent = `${at}%`;
  el("mute").classList.toggle("muted", silent);
  el("mute").setAttribute("aria-pressed", silent ? "true" : "false");
  // Not awaited and nothing to report: the level is a convenience, and an
  // engine that did not take it plays at the one it already had.
  if (invoke) invoke("set_volume", { level: silent ? 0 : gain(at) }).catch(() => {});
  if (!remember) return;
  prefs.set("volume", at);
  prefs.set("muted", silent);
}

/// Move the level, as a hand does. It comes off ミュート on the way past --
/// somebody turning it up is asking to hear something -- except at the
/// bottom of the travel, where the two answers say the same thing and there
/// is nothing to come off for.
const takeVolume = (at) => showVolume(at, muted() && Math.round(Number(at)) <= 0);

el("volume").addEventListener("input", () => takeVolume(el("volume").value));

el("mute").addEventListener("click", () => showVolume(el("volume").value, !muted()));

showVolume(prefs.get("volume"), !!prefs.get("muted"), false);

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
    el("status").textContent = tr("editor.sceneFailed", { e });
  }
}

/// What the pass over the recording made, under the film strip. Cut short the
/// same way and for the same reason -- see `.strip-foot #warm`.
function showWarm(text) {
  const e = el("warm");
  e.textContent = text;
  e.title = text;
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
  cardGuesses.clear();
  el("prev-scene").disabled = true;
  el("next-scene").disabled = true;
  showWarm(tr("warm.start"));
  try {
    const r = await invoke("prepare");
    const tk = r.track;
    scenes = tk.scenes;
    interval = tk.interval;
    warmed = true;
    held = true;
    proxied = !!r.proxy;
    el("prev-scene").disabled = false;
    el("next-scene").disabled = false;
    const made = [];
    if (r.proxy) {
      made.push(
        tr("warm.proxy", {
          w: r.proxy.width,
          h: r.proxy.height,
          mb: (r.proxy.bytes / 1e6).toFixed(0),
          how: tr(r.proxy.cached ? "warm.proxyReused" : "warm.proxyBuilt"),
          s: r.proxy.seconds.toFixed(0),
        })
      );
    }
    // A recording with no proxy is the ordinary case, so it is not worth
    // saying. A proxy that was asked for and failed is.
    if (r.note) made.push(tr("warm.noProxy", { note: r.note }));
    if (r.index) {
      // The seconds are the thumbnail pass's, which is the whole of what the
      // index cost only when there is no proxy -- with one, the same number
      // is already reported above and saying it twice reads as twice the wait.
      const how = r.index.cached
        ? tr("warm.indexReused")
        : r.proxy
          ? ""
          : tr("warm.indexBuilt", { s: tk.seconds.toFixed(0) });
      made.push(tr("warm.index", { mb: (r.index.bytes / 1e6).toFixed(0), how }));
    } else {
      made.push(tr("warm.noIndex"));
    }
    made.push(tr("warm.thumbs", { n: tk.thumbs, gap: tk.interval.toFixed(2) }));
    made.push(tr("warm.scenes", { n: tk.scenes.length }));
    showWarm(made.join(" / "));
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
    showWarm(tr("warm.failed", { e }));
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
  el("hover-kind").textContent = held ? "" : tr("editor.hoverWarming");
  if (!held) return;
  clearTimeout(hoverTimer);
  const token = ++hoverToken;
  hoverTimer = setTimeout(async () => {
    const shot = await invoke("hover_thumb", { time: outToSrc(o) });
    if (token !== hoverToken || !shot) return;
    el("hover-img").src = shot.url;
    el("hover-kind").textContent = nearScene(shot.time, 1.2) ? tr("editor.hoverScene") : "";
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

/// Which recompute is the current one, and the wait before the panel admits
/// that it is out of date.
///
/// An answer that arrives after a newer question was asked is thrown away:
/// two cuts in quick succession would otherwise leave the first one's numbers
/// standing under the second one's timeline. The fade waits a moment before
/// it starts, so a plan that comes straight back -- which is most of them --
/// does not blink.
let planRun = 0;
let planFade = null;

function planSettled() {
  clearTimeout(planFade);
  planFade = null;
  el("plan-panel").classList.remove("stale");
}

async function refreshPlan() {
  // Whichever question this call asks, it is now the only one whose answer
  // this panel will take.
  const run = ++planRun;
  // What a plan says is which stretches copy and which are re-encoded, and
  // that is a question about where the access points are. Until the walk has
  // found them there is no answer to give, and asking for one would only get
  // "no file open" back.
  //
  // `opening` is that same wait one step earlier: the window is up and a
  // recording is on its way into it, but `open_outline` has not answered yet,
  // so `src` is either still null or -- on a reopen -- still the last
  // recording's. Either way the band read 「ファイルを開いてください」 over a
  // window that was reading a file. Only the genuinely empty editor asks for
  // one.
  if (opening || (src && !walked())) {
    planSettled();
    el("plan-text").textContent = tr("plan.reading");
    el("segments").innerHTML = "";
    el("copied-bar").style.width = "0%";
    el("copied-bar").parentElement.classList.add("unknown");
    el("smart-badge").textContent = "—";
    return;
  }
  el("copied-bar").parentElement.classList.remove("unknown");
  const ranges = outputRanges();
  if (!src || !ranges.length) {
    planSettled();
    el("plan-text").textContent = tr(src ? "plan.allCut" : "plan.openFile");
    el("segments").innerHTML = "";
    el("copied-bar").style.width = "0%";
    el("smart-badge").textContent = "—";
    return;
  }
  planFade = setTimeout(() => {
    if (run === planRun) el("plan-panel").classList.add("stale");
  }, 200);
  try {
    const p = await invoke("make_plan", { ranges });
    // A cut made while this was out asked its own question, and that one's
    // answer is the one this panel is waiting for.
    if (run !== planRun) return;
    const pct = p.total > 0 ? (100 * p.copied) / p.total : 0;
    el("copied-bar").style.width = `${pct}%`;
    const redone = p.segments
      .filter((g) => g.kind !== "copy")
      .reduce((n, g) => n + g.frames, 0);
    el("plan-text").textContent = tr("plan.text", {
      total: fmt(p.total),
      ranges: ranges.length,
      cuts: cuts.length,
      copied: p.copied.toFixed(2),
      pct: pctText(pct, redone),
      reencoded: p.reencoded.toFixed(2),
    });
    // "Completely lossless" has to mean not one re-encoded picture, not a
    // percentage that rounds to a hundred: a cut off an access point always
    // re-encodes a frame or two, and 2 frames out of 40000 rounds to 100.0%.
    el("smart-badge").textContent =
      redone === 0 ? tr("plan.lossless") : tr("plan.reencoded", { n: redone });
    el("segments").innerHTML = p.segments
      .map(
        (s) =>
          `<li class="${s.kind}">${tr(s.kind === "copy" ? "plan.segCopy" : "plan.segEncode")} ` +
          `${fmt(s.start)} → ${fmt(s.end)}  (${tr("out.ovlNote", { n: s.frames })})</li>`
      )
      .join("");
    // Whether the box holds all of it. Three lines is what an ordinary cut
    // costs and what the box was sized for, but two cuts costs five -- a
    // copy, then a re-encode and a copy at each seam -- and two cuts is what
    // taking the breaks out of a recording comes to. The desktop's own
    // scrollbar is an overlay that is invisible until it is used, so without
    // this the first three of five lines look like the whole plan.
    const box = el("segments");
    box.parentElement.classList.toggle("more", box.scrollHeight > box.clientHeight + 1);
  } catch (e) {
    if (run !== planRun) return;
    el("plan-text").textContent = tr("plan.failed", { e });
  } finally {
    if (run === planRun) planSettled();
  }
}

// --- edit ---------------------------------------------------------------

// IN..OUT is half-open, so the picture at OUT survives. That is right in the
// middle of a recording and wrong at its ends: there is no position past the
// last picture to put OUT at, so "cut to the end" would always leave that one
// picture behind -- a stray frame at the end of the output. The two ends
// therefore snap to the bounds of the timeline.
const atFirstPicture = (o) => outToSrc(o) <= headTime() + frame() / 2;
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
/// How many marks it put down, so that the caller knows whether there was a
/// list beside the recording at all: a disc's chapters are the answer when
/// there was not, and would be noise on top of a list somebody has kept.
async function loadSidecarKeyframes() {
  return (await readKeyframeFile(markPath("keyframe"))) || 0;
}

/// A keyframe list, wherever it is, onto the marks.
///
/// How many it put down; `null` where the file could not be read, which it
/// has already said in the status line. Not the same answer as a file that
/// was read and held nothing -- only the caller that went looking for a file
/// nobody asked for can treat the two alike.
async function readKeyframeFile(path) {
  let frames;
  try {
    frames = await invoke("read_keyframes", { path });
  } catch (e) {
    el("status").textContent = tr("keyframes.readFailed", { e });
    return null;
  }
  if (!frames) return 0;
  const times = frames
    .map((n) => n / src.fps)
    .filter((o) => o <= outDur + 1e-6)
    .map(outToSrc);
  if (!times.length) return 0;
  addKeyframes(times);
  el("status").textContent = tr("keyframes.read", {
    n: liveKeyframes().length,
    file: leaf(path),
  });
  return times.length;
}

// --- mark files ---------------------------------------------------------
//
// What was found in a recording, written down beside it: the places worth
// coming back to, or the cut itself. Two shapes, because two other programs
// read them -- a `.keyframe` is a list of frame numbers and nothing else,
// which is what the reference tool writes; an AviSynth `Trim` line is the
// ranges that survive, which is what a script wants.
//
// Both count frames from the recording's first picture, and both are read
// back the same way. A number in a mark file counts pictures in the file it
// is lying next to: these lie next to the recording, and the list the export
// writes lies next to the output and counts that.

/// Where a mark file for this recording goes.
///
/// Two conventions, and they are not ours to reconcile. The keyframe list
/// drops the recording's extension, the way the reference tool writes one:
/// `録画.ts` becomes `録画.keyframe`. The Trim line keeps it --
/// `録画.ts.trim.avs` -- because that is the name the AviSynth side of the
/// world puts beside a recording and looks for again.
function markPath(kind) {
  const base = sideBase || (src ? src.path.replace(/\.[^./\\]*$/, "") : "");
  if (kind === "keyframe") return `${base}.keyframe`;
  const ext = src ? (src.path.match(/\.[^./\\]*$/) || [""])[0] : "";
  return `${base}${ext}.trim.avs`;
}

/// The marks on screen, as frame numbers against the recording.
const markNumbers = () =>
  liveKeyframes()
    .map((t) => Math.round((t - headTime()) * src.fps))
    .filter((n) => n >= 0);

/// What survives the cuts, as an AviSynth line.
///
/// `Trim(a,b) ++ Trim(a,b)`, both ends inclusive, which is what Trim means by
/// them. One line and nothing else: whatever reads this next has to find the
/// ranges in it, and a header with this program's name in it is one more
/// thing for it to trip over.
function trimBody() {
  const head = headTime();
  const no = (t) => Math.round((t - head) * src.fps);
  const parts = [];
  for (const k of keeps) {
    const a = Math.max(0, no(k.a));
    const b = no(k.b) - 1;
    if (b >= a) parts.push(`Trim(${a},${b})`);
  }
  return `${parts.join(" ++ ")}\r\n`;
}

/// Which shape a name asks for. The extension, which is what somebody typing
/// one by hand means by typing it.
const kindOf = (path) => (/\.avs$/i.test(path) ? "trim" : "keyframe");

/// Write the marks beside the recording.
///
/// `ask` puts a picker up with the name already in it, which is what the
/// button's menu does once the shape has been chosen on it.
///
/// Without `ask` the file goes straight to that name in that shape, which is
/// the shortcut and the point of having one. A picker does its own asking
/// about a file that is already there, so only the shortcut has that question
/// left to answer, and 環境設定 says whether it is even asked. See
/// `quietOverwrite`.
async function saveMarks(kind, ask) {
  if (!src) return;
  let to = markPath(kind);
  if (ask) {
    if (!dialog) return;
    const picked = await dialog.save({
      defaultPath: to,
      // The one shape that was chosen, so that the type the dialog shows and
      // the name it is about to write say the same thing. Offering both here
      // put `AviSynth スクリプト` in the name and `キーフレーム情報` in the
      // type list, which is the dialog disagreeing with itself.
      filters: [
        {
          name: tr(kind === "keyframe" ? "marks.kind.keyframe" : "marks.kind.trim"),
          extensions: kind === "keyframe" ? ["keyframe"] : ["avs"],
        },
      ],
    });
    if (!picked) return;
    to = picked;
    // The name has the last word about which shape it is: somebody who typed
    // `.avs` over the name meant the Trim line, whichever entry opened the
    // dialog. A name that says neither is written as it was asked for.
    if (/\.(keyframe|avs)$/i.test(to)) kind = kindOf(to);
  } else if (!prefs.get("quietOverwrite")) {
    let there = null;
    try {
      there = await invoke("read_sidecar", { path: to });
    } catch {
      // Unreadable is not the same as not there, and the write below says so
      // properly. Nothing to ask about here.
    }
    if (there !== null && dialog) {
      const go = await dialog.ask(tr("marks.overwriteBody", { file: leaf(to) }), {
        title: tr("marks.overwriteTitle"),
        kind: "warning",
      });
      if (!go) return;
    }
  }
  try {
    let n;
    if (kind === "keyframe") {
      n = await invoke("write_keyframes", { path: to, frames: markNumbers(), fps: src.fps });
    } else {
      await invoke("write_sidecar", { path: to, body: trimBody() });
      n = keeps.length;
    }
    el("status").textContent = tr(kind === "keyframe" ? "marks.saved" : "trim.saved", {
      n,
      file: leaf(to),
    });
  } catch (e) {
    el("status").textContent = tr("marks.saveFailed", { e });
  }
}

/// The name at the end of a path, for a line that has a window's width to say
/// it in.
const leaf = (path) => path.split(/[/\\]/).pop();

/// Read marks out of a file somebody points at.
///
/// The picker rather than the name beside the recording: a list kept under
/// another recording's name, or one brought over from another machine, is
/// exactly what it is for. Which shape is being read comes from the line that
/// was picked -- except that a name ending the other way is taken at its
/// word, the same as saving.
///
/// What arrives is read as the file beside the recording is read, so a list
/// written here and a list written by the reference tool land on the same
/// pictures. A keyframe list adds its marks to the ones already up; a Trim
/// line *cuts*, and lands in the undo history like any other cut.
async function loadMarksFrom(kind) {
  if (!src || !dialog) return;
  const picked = await dialog.open({
    multiple: false,
    defaultPath: markPath(kind),
    filters: [
      {
        name: tr(kind === "keyframe" ? "marks.kind.keyframe" : "marks.kind.trim"),
        extensions: kind === "keyframe" ? ["keyframe"] : ["avs"],
      },
    ],
  });
  if (!picked) return;
  const from = Array.isArray(picked) ? picked[0] : picked;
  if (/\.(keyframe|avs)$/i.test(from)) kind = kindOf(from);
  const got = kind === "keyframe" ? await readKeyframeFile(from) : await readTrimFile(from);
  // Nothing in it, as against unreadable: the reader has said its piece about
  // the second, and a picker that answers a deliberate choice with silence
  // looks like a program that did not hear the click.
  if (got === 0 || got === false) {
    el("status").textContent = tr("marks.readNone", { file: leaf(from) });
  }
}

/// The marks off the timeline, and nothing else.
///
/// 全消去 is the other answer and has its own button: it takes the cuts with
/// it, which is not what somebody redoing the marks on an edit they have kept
/// is asking for.
function clearKeyframes() {
  if (!keyframes.length) return;
  remember();
  keyframes = [];
  activeKey = null;
  renderKeyframes();
  draw();
  scheduleStrip();
}

// --- the menu at the end of the transport row ---------------------------
//
// What is behind it is everything about the marks that is not a control: the
// two mark files both ways, the disc's own chapters, and clearing the marks.
// Each is reached once in a session, and a button each along the foot of the
// window would crowd out the cutting.
//
// The save lines ask for a name, and the shape is chosen by picking a line
// rather than in the dialog's type list: the dialog is the platform's, and
// GTK's does not rewrite the name when the type beside it changes -- so a
// shape chosen there would look chosen and write the other one. Measured, not
// assumed: picking 「AviSynth スクリプト」 left `short.keyframe` standing in
// the name field. The shortcuts skip both steps.

const moreMenu = () => el("more-menu");

/// Put the menu up or away, and as it goes up say on each line whether it can
/// do anything.
///
/// Read as it opens rather than kept in step with every cut: a menu that is
/// only ever looked at while it is open only has to be right then. A line
/// that cannot run is greyed and stays where it is -- a menu whose items come
/// and go is a menu that has to be read from the top every time.
function showMore(on) {
  if (on) {
    for (const id of ["load-keyframe", "load-trim", "save-as-keyframe", "save-as-trim"]) {
      el(id).disabled = !src;
    }
    el("chapter-keys").disabled = !src || !discChapters.length;
    el("clear-keys").disabled = !src || !keyframes.length;
  }
  moreMenu().hidden = !on;
  el("more").setAttribute("aria-expanded", String(!!on));
  el("more").classList.toggle("open", !!on);
}

el("more").addEventListener("click", (ev) => {
  ev.stopPropagation();
  showMore(moreMenu().hidden);
});
el("load-keyframe").addEventListener("click", () => {
  showMore(false);
  loadMarksFrom("keyframe");
});
el("load-trim").addEventListener("click", () => {
  showMore(false);
  loadMarksFrom("trim");
});
el("save-as-keyframe").addEventListener("click", () => {
  showMore(false);
  saveMarks("keyframe", true);
});
el("save-as-trim").addEventListener("click", () => {
  showMore(false);
  saveMarks("trim", true);
});
el("chapter-keys").addEventListener("click", () => {
  showMore(false);
  applyDiscChapters(discChapters);
});
el("clear-keys").addEventListener("click", () => {
  showMore(false);
  clearKeyframes();
});
// Anywhere else, and it is gone -- including the wheel, which moves the
// playhead under a menu that would otherwise stay put over it.
window.addEventListener("click", () => showMore(false));
window.addEventListener("wheel", () => showMore(false), true);

/// The cuts a Trim line describes, as source ranges taken out.
///
/// A Trim line says what *survives*, so the cuts are everything else: the
/// gaps between the ranges and the head and tail outside them. `Trim(a,b)`
/// with a negative second number is AviSynth's other spelling -- a length
/// rather than an end -- and is read as one.
///
/// `null` when the text held no ranges at all, which is not the same answer
/// as a line that keeps the whole recording.
function trimCuts(body) {
  const head = headTime();
  const at = (n) => head + n / src.fps;
  const kept = [];
  for (const m of body.matchAll(/\bTrim\s*\(\s*(\d+)\s*,\s*(-?\d+)\s*\)/gi)) {
    const a = Number(m[1]);
    const end = Number(m[2]);
    const b = end < 0 ? a - end - 1 : end;
    if (b >= a) kept.push({ a: at(a), b: at(b + 1) });
  }
  if (!kept.length) return null;
  const out = [];
  let pos = head;
  for (const k of normalise(kept)) {
    if (k.a > pos + frame() / 2) out.push({ a: pos, b: k.a });
    pos = Math.max(pos, k.b);
  }
  if (pos < src.duration - frame() / 2) out.push({ a: pos, b: src.duration });
  return out;
}

/// Pick up a Trim line saved beside the recording, if there is one.
///
/// Unlike the keyframe list this one *cuts*: the file is the edit, and
/// opening the recording again should find it as it was left. Whether that
/// happens when a keyframe list is sitting there too is 環境設定.
async function loadSidecarTrim() {
  return (await readTrimFile(markPath("trim"))) === true;
}

/// An AviSynth Trim line, wherever it is, onto the timeline.
///
/// True when it cut, false when there was nothing in it to cut by, `null`
/// where it could not be read.
async function readTrimFile(path) {
  let body;
  try {
    body = await invoke("read_sidecar", { path });
  } catch (e) {
    el("status").textContent = tr("trim.readFailed", { e });
    return null;
  }
  if (body === null || body === undefined) return false;
  const out = trimCuts(body);
  if (!out) return false;
  // An empty list is a Trim line that keeps the whole recording. Nothing to
  // do, and still an answer: the file was there and it has been read.
  if (out.length) applyCuts(out);
  el("status").textContent = tr("trim.read", { n: out.length, file: leaf(path) });
  return true;
}

/// The mark files beside the recording, if there are any.
///
/// The two do not say the same thing -- a `.keyframe` is a list of places and
/// leaves the timeline whole, a Trim line is the cut itself -- so when both
/// are there only one is read, and which is 環境設定. Reading both would put
/// marks on a timeline that had already closed over the material they point
/// at. Either on its own is read whatever the preference says.
///
/// Answers whether anything was found: a disc's own chapters are what fills
/// an empty timeline otherwise, and they would be noise on top of a list
/// somebody has kept.
async function loadMarkFiles() {
  const order =
    prefs.get("sidecarPriority") === "trim" ? ["trim", "keyframe"] : ["keyframe", "trim"];
  for (const kind of order) {
    const read = kind === "trim" ? await loadSidecarTrim() : (await loadSidecarKeyframes()) > 0;
    if (read) return true;
  }
  return false;
}

/// The chapter points a recorder wrote on a BDAV disc, as marks.
///
/// On a Japanese recording these are frequently the commercial breaks
/// themselves, which is the same thing the detection spends minutes looking
/// for -- and here it is written down, exactly, by the machine that made the
/// recording. So a disc opens with its own chapters already on the timeline.
///
/// They arrive on the stream's own clock, because that is the clock the
/// playlist counts in. Everything in this window is rebased to the
/// container's start, so that subtraction is the whole of the conversion.
///
/// Not snapped to an access point, the same as the detection's marks: a mark
/// says where the chapter is, and moving it onto the nearest lossless point
/// is a separate decision with its own button. Marks that land outside the
/// material are dropped rather than clamped -- a mark in the wrong place is
/// worse than no mark, which is what the disc reader says about them too.
function applyDiscChapters(chapters) {
  if (!src || !chapters || !chapters.length) return 0;
  const first = headTime();
  const times = chapters
    .map((t) => t - src.start_time)
    .filter((t) => t >= first - 0.5 && t <= src.duration + 1e-6)
    .map((t) => Math.max(t, first));
  if (!times.length) return 0;
  addKeyframes(times);
  el("status").textContent = tr("keyframes.chapters", { n: times.length });
  return times.length;
}

/// The sound track this window's line speaks for: the first one the track
/// menu has left switched on.
///
/// Not the main track. libavformat calls the widest track the main one, and
/// on a pressed disc that is the English 5.1 sitting beside the Japanese
/// stereo -- so a recording opened with only the second kept would still read
/// 5.1 here. The clip list asks the same question of a row it has not opened;
/// see `audioOf` there.
///
/// Null where there is no sound, and where every track of it was switched
/// off.
function keptAudio() {
  if (!src || !src.has_audio) return null;
  const tracks = src.audio_tracks || [];
  // Read by a version that did not list the tracks: the main track is all
  // there is to go on.
  if (!tracks.length) return { channels: src.audio_channels || 0 };
  return tracks.find((a) => !dropStreams.includes(a.index)) || null;
}

/// The note under the timeline. It is the one thing in that bar allowed to be
/// cut short when the window is narrow, so it carries the whole of itself in
/// its tooltip -- see `.statusbar #cm-note`.
function showCmNote(text) {
  const e = el("cm-note");
  e.textContent = text;
  e.title = text;
}

/// The recording's name in the title bar of the window's own header, and its
/// shape on the line under it.
///
/// Its own function because it is said twice: once when the recording is
/// opened, and again if the language changes while it is up.
/// Switch off what the walk has not made possible yet, and on again when it
/// has.
///
/// Two things, now. 無劣化点へ吸着 has nothing to snap to until there are
/// access points, and playback decodes continuously, so an approximate seek
/// gives it no reliable place to start.
///
/// The track list and the commercial detection used to be here too, for a
/// reason that turned out not to be one: they were reading the recording
/// through the opened [`Source`], not through anything in it that the walk
/// produces. The tracks are named by the container, and the three passes a
/// detection makes -- captions, audio, logo -- never look at an access point.
/// Both answer from the container's own answer now, so both work from the
/// moment the window opens.
///
/// Everything else in the window -- the timeline, the cards, the marks,
/// cutting itself -- works on times, and times are known from the moment the
/// container was opened.
///
/// The scene buttons are not here: they wait on the pictures rather than on
/// the points, and `prepare` turns them on.
function paintReadiness() {
  const yet = walked();
  for (const id of ["play", "loop", "rewind", "fast-fwd", "snap"]) el(id).disabled = !yet;
}

function paintSourceInfo() {
  if (!src) return;
  paintReadiness();
  const flags = [
    tr(src.interlaced ? "media.interlaced" : "media.progressive"),
    src.pulldown ? tr("media.pulldown") : null,
  ].filter(Boolean);
  const sound = keptAudio();
  // What the *recording* carries, not what the output is to keep: the
  // preview plays the recording's own sound whichever tracks are ticked for
  // writing, so a bilingual programme with its dub left out still has a
  // level worth setting.
  for (const id of ["mute", "volume"]) el(id).disabled = !src.has_audio;
  el("title").textContent = shownName || src.path.split(/[/\\]/).pop();
  el("info").textContent = tr("editor.info", {
    // A dash rather than zero while the walk is still counting them: "無劣化
    //点: 0" about a recording with three thousand of them is worse than
    // saying nothing.
    points: walked() ? src.points.length : "—",
    w: src.width,
    h: src.height,
    fps: src.fps.toFixed(2),
    flags: flags.join(" "),
    audio: sound
      ? tr("editor.infoAudioYes") + (sound.channels ? ` (${chLabel(sound.channels)})` : "")
      : tr("editor.infoAudioNo"),
    codec: src.codec,
    unusable: src.unusable_points ? tr("editor.infoUnusable", { n: src.unusable_points }) : "",
  });
}

/// Load `picked` into the editor, putting `saved` back where there is any.
///
/// `saved` is what [`captureEdit`] handed the clip list the last time this
/// recording left the editor. Restoring it is what makes the list a place
/// you can go back to: cuts and marks belong to the clip, not to the one
/// session that happened to be looking at it.
///
/// The sidecar marks are read only on a first visit. On a return the list
/// already holds marks that have been worked on, and re-reading the file
/// beside the recording would add its own on top of them. The disc's own
/// chapter points are put down under the same rule, and only where there was
/// no list beside the recording: a `.keyframe` file is somebody's answer,
/// and the disc's is the answer when nobody has given one.
async function openPath(picked, saved, side, name, chapters, dropPids) {
  jlog(`openPath ${picked}`);
  if (!picked) return;
  shownName = name || null;
  sideBase = side || picked.replace(/\.[^./\\]*$/, "");
  // Held for the menu, which can put them down again after they have been
  // cleared -- and for the ordinary way in, a few lines below, where they
  // fill a timeline no file beside the recording had anything to say about.
  discChapters = chapters || [];
  el("title").textContent = tr("editor.analysing");
  // The band comes up out of the markup saying 「ファイルを開いてください」,
  // which stops being true here rather than when `src` lands.
  schedulePlan();
  try {
    // The container's own answer first, which costs one open. It has
    // everything this window draws with except where the access points are --
    // the length, the picture size, the sound, the clock -- so the timeline,
    // the scrubber and the cards can all be up while the walk that finds the
    // points is still reading the recording. That walk is a second a
    // gigabyte, and over a share it is the difference between a window you
    // can work in and half a minute of its own name.
    //
    // `open_source` is the same question asked properly, and it is started
    // here and picked up at the end of this: everything between is set up
    // that does not depend on the answer.
    const exact = invoke("open_source", { path: picked });
    // A failure has to be somebody's, or the promise is unhandled and the
    // window gets a console error instead of a message. Answered again below.
    exact.catch(() => {});
    src = await invoke("open_outline", { path: picked });
    paintSourceInfo();
    paintSubsPicker();
    cuts = saved ? saved.cuts.map((c) => ({ a: c.a, b: c.b })) : [];
    past = [];
    undone = [];
    keyframes = saved ? saved.keyframes.slice() : [];
    activeKey = saved ? saved.activeKey : null;
    cmBlocks = saved ? saved.cmBlocks || [] : [];
    cmSummary = saved ? saved.cmNote || "" : "";
    dropStreams = saved ? (saved.dropStreams || []).slice() : [];
    trackList = null;
    // A first visit to a recording that came off a disc starts from the
    // answer given when the disc was read. That answer is in PIDs, because it
    // was given before anything was open; here something is, so it becomes
    // what the rest of this window speaks in -- and from now on this window's
    // answer is the one that counts.
    //
    // A PID can name more than one stream. A Blu-ray's lossless sound arrives
    // as a TrueHD track with an AC-3 track folded into it, both on the one
    // PID and both handed over separately, so switching that track off has to
    // switch off both halves of it.
    if (!saved && dropPids && dropPids.length) {
      try {
        trackList = await invoke("tracks", { path: picked });
        dropStreams = trackList
          .filter((k) => k.optional && dropPids.includes(k.pid))
          .map((k) => k.index);
      } catch (e) {
        jlog(`tracks for the disc's choice: ${e}`);
      }
    }
    paintTrackButton();
    // Said again now the tracks are settled: the line above was drawn before
    // the disc's answer had been turned into stream indices, and which track
    // it speaks for depends on that.
    paintSourceInfo();
    stripCache = null;
    stripShots = [];
    forgetGlances();
    shownTime = -1;
    hideHover();
    // What the line under the strip says is about a recording that has been
    // read through, so on the way into another one it is about the wrong
    // recording. `prepare` writes it again on the far side of the walk.
    showWarm("");
    rebuildTimeline();
    paintHistory();
    showCmNote(cmSummary);
    renderKeyframes();
    // Opened whole: nothing is cut yet, so the selection is the recording.
    selA = saved ? Math.min(saved.selA, outDur) : 0;
    selB = saved ? Math.min(saved.selB, outDur) : outDur;
    el("status").textContent = "";
    // The files beside the recording, before the walk rather than after it.
    // Their numbers count pictures from the recording's first one, so what
    // they need is the head -- and the outline now says where that is, read
    // off the front of the file. So the marks are on the timeline and the
    // list of them is on the left while the walk is still reading, which on a
    // recording over a share is most of a minute of having them. See
    // `headTime` and `smartcut_core::first_picture`.
    //
    // A recording whose opening held no picture to find is the case the head
    // is unknown for. Its marks would land the second or so early that the
    // clock's own zero puts them, so that one waits for the walk after all.
    const early = !saved && src.points.length === 0 && src.head !== null;
    let marks = false;
    if (early) marks = await settle(() => loadMarkFiles());
    await showFrame(saved ? saved.playhead : 0);
    schedulePlan();
    // And now the walk, which has been running behind all of the above.
    // Everything on screen is right without it; what it adds is exactness --
    // where a cut is free, how the strip divides, and a stage picture that is
    // the frame asked for rather than the nearest one a container seek could
    // find.
    await pointsArrived(exact, picked);
    if (!saved && !early) marks = await settle(() => loadMarkFiles());
    // The disc's own chapters, which fill a timeline no file beside the
    // recording had anything to say about. Left until here either way: they
    // arrive on the stream's own clock and are dropped rather than clamped
    // where they fall outside the material, which is a question for the walk.
    if (!saved && !marks) await settle(() => applyDiscChapters(discChapters));
    prepare();
    // Asked again now that the open is over. Everything above schedules the
    // plan while this window is still `opening`, and a plan asked for then is
    // answered with 「読み込み中」 and nothing else -- which is the honest
    // answer at the time, and the last one the panel would ever get: the walk
    // has already landed, so nothing further is coming to ask on its behalf.
    // The wait between them is a picture decode, so whether the panel settled
    // came down to whether that decode beat a 120 ms timer, and on a 1440x1080
    // recording in a wide window it did not.
    schedulePlan();
  } catch (e) {
    el("title").textContent = "";
    el("status").textContent = tr("editor.openFailed", { e });
    throw e;
  }
}

/// Take the walk's answer when it lands, and redraw what only it can settle.
///
/// The cuts are not touched. They are times, and they were times before this;
/// what changes is that the timeline now knows which of those times are free
/// and can snap to them. A cut made during the wait is a cut at the instant
/// it was made -- the planner clamps it to an access point when the output is
/// written, exactly as it would for one made after.
///
/// `picked` is checked against what came back because a second recording can
/// be opened in this window while the first one's walk is still running, and
/// the answer then belongs to nobody.
async function pointsArrived(exact, picked) {
  let full;
  try {
    full = await exact;
  } catch (e) {
    // The walk is where a recording that cannot be read says so. The outline
    // opened it, so this is rare -- a file removed under the window, a disc
    // ejected -- but it is not a window to go on cutting in.
    el("status").textContent = tr("editor.openFailed", { e });
    throw e;
  }
  if (!src || src.path !== picked) return;
  const at = playhead;
  src = full;
  paintSourceInfo();
  paintSubsPicker();
  rebuildTimeline();
  renderKeyframes();
  stripCache = null;
  stripShots = [];
  // The pictures they hold are approximate, and there is now an exact answer
  // for every one of them.
  forgetGlances();
  shownTime = -1;
  draw();
  scheduleStrip();
  // And the plan, which had nothing to say until now: what copies and what
  // is re-encoded is entirely a question of where the access points are.
  schedulePlan();
  // The picture on the stage came from a container seek and is a second or
  // two out. Now that the points are here it can be the frame it says it is.
  await showFrame(at);
}

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
el("undo-cut").addEventListener("click", () => stepHistory(past, undone));
el("redo-cut").addEventListener("click", () => stepHistory(undone, past));
el("clear-all").addEventListener("click", () => {
  if (!src) return;
  // The one button in the row that used to be a one-way door: it emptied the
  // history along with everything else. It is a step like any other now.
  remember();
  cuts = [];
  keyframes = [];
  activeKey = null;
  rebuildTimeline();
  selA = 0;
  selB = outDur;
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

/// What a page key does for the modifier it was pressed with: a jump of `by`
/// seconds, or a scroll at `rate` seconds a second. Only ever one of the two,
/// and neither for a preference somebody has emptied -- which is then a key
/// that does nothing rather than a key that jumps to the start.
///
/// Four answers, one per way the pair can be pressed, and each is a number
/// and the unit it is counted in. Pictures and seconds are amounts: the key
/// moves that far, once per press. A percent is a speed: a share of
/// `SCROLL_MAX_RATE`, held for as long as the key is. A quarter of it is
/// fifteen times the recording's own speed, so a second of holding covers
/// fifteen seconds of the recording, whatever is open and however much of it
/// the cuts have taken out.
///
/// Read at the keystroke rather than held in a variable, so a number changed
/// in the other window is in force at the next press, and under a key that is
/// already down at the next tick of the scroll.
function pageStep(ev) {
  const held = ev.ctrlKey || ev.metaKey;
  const which = held
    ? ev.shiftKey
      ? "pageStepShiftCtrl"
      : "pageStepCtrl"
    : ev.shiftKey
      ? "pageStepShift"
      : "pageStep";
  const by = Number(prefs.get(which));
  if (!isFinite(by) || by <= 0) return { by: 0, rate: 0 };
  const unit = prefs.get(`${which}Unit`);
  if (unit === "frame") return { by: by * frame(), rate: 0 };
  if (unit === "pct") return { by: 0, rate: (SCROLL_MAX_RATE * by) / 100 };
  return { by, rate: 0 };
}

window.addEventListener("keydown", (ev) => {
  // A panel over the timeline is the program not listening to the timeline:
  // space would start playback behind it, and the arrow keys would step the
  // playhead nobody can see. The save menu is a panel like any other, and
  // Escape there puts the menu away rather than the window.
  if (!moreMenu().hidden) {
    if (ev.key === "Escape") {
      ev.preventDefault();
      showMore(false);
    }
    return;
  }
  if (!el("tracks-modal").hidden) {
    if (ev.key === "Escape") el("tracks-modal").hidden = true;
    return;
  }
  if (ev.target.tagName === "INPUT" || ev.target.tagName === "SELECT") return;
  // Escape leaves the window, and leaves it the way キャンセル does: the cuts
  // made in here are dropped. Asked for, and settled deliberately -- this is
  // the one key in the window that can lose an evening's work. Nothing is
  // lost that was not made in this window: the list keeps what it had before.
  if (ev.key === "Escape") {
    ev.preventDefault();
    cancelEdit();
    return;
  }
  if (!src) return;
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
  // Both spellings of やり直し: Ctrl+Y is the one Windows programs are worked
  // by, Ctrl+Shift+Z the one editors are.
  if ((ev.ctrlKey || ev.metaKey) && (ev.key === "z" || ev.key === "Z")) {
    ev.preventDefault();
    if (ev.shiftKey) stepHistory(undone, past);
    else stepHistory(past, undone);
    return;
  }
  if ((ev.ctrlKey || ev.metaKey) && (ev.key === "y" || ev.key === "Y")) {
    ev.preventDefault();
    stepHistory(undone, past);
    return;
  }
  // The mark files: H writes one, L reads one, and Shift picks the Trim line
  // out of the pair either way.
  //
  // H rather than S because that is the key the reference tool writes a
  // keyframe list with, and this window is laid out after that tool. It also
  // leaves Ctrl+S meaning one thing across the program: the list window saves
  // the project with it.
  //
  // Writing goes straight to the name beside the recording without a picker,
  // which is the point of a shortcut; whether it stops to ask before writing
  // over one is 環境設定 -- see `saveMarks`. Reading does put the picker up,
  // and has to: the file being read is frequently not the one beside the
  // recording, and that one is read on the way in without being asked for.
  if ((ev.ctrlKey || ev.metaKey) && (ev.key === "h" || ev.key === "H")) {
    ev.preventDefault();
    saveMarks(ev.shiftKey ? "trim" : "keyframe", false);
    return;
  }
  if ((ev.ctrlKey || ev.metaKey) && (ev.key === "l" || ev.key === "L")) {
    ev.preventDefault();
    loadMarksFrom(ev.shiftKey ? "trim" : "keyframe");
    return;
  }
  // PageUp and PageDown move by whatever 環境設定 says they move by, and
  // they are the one pair of keys a modifier changes the *amount* for rather
  // than the meaning of. Shift, Ctrl and the two together each carry their
  // own answer; see `pageStep`.
  //
  // An answer counted in pictures or seconds is a jump, one per press. An
  // answer counted in a share of the timeline is a speed, and the key scrolls
  // for as long as it is held -- the same scroll the strip's right drag runs,
  // held pictures and all, at a speed somebody has set rather than one the
  // pointer is deciding.
  if (ev.key === "PageUp" || ev.key === "PageDown") {
    ev.preventDefault();
    const dir = ev.key === "PageDown" ? 1 : -1;
    const { by, rate } = pageStep(ev);
    if (rate > 0) {
      if (playing) stopPlay();
      startPageScroll(dir, ev);
      return;
    }
    // A modifier taken off or added while the key is still down, where what
    // it now says is an amount: the speed it was scrolling at is not this
    // key's answer any more, so the scroll stops where it got to and the
    // press is a jump like any other.
    if (search && search.page) endScroll();
    if (by > 0) {
      if (playing) stopPlay();
      scrubTo(playOut() + dir * by);
    }
    return;
  }
  if (ev.ctrlKey || ev.metaKey || ev.altKey) return;
  // ミュート, and one of the few keys that does not stop playback: it is
  // about the sound rather than about where the playhead is, and it is
  // pressed *because* something is playing.
  if (ev.key === "m" || ev.key === "M") {
    ev.preventDefault();
    if (!el("mute").disabled) el("mute").click();
    return;
  }
  // A letter, whichever case it arrives in: Shift held and Caps on are the
  // same key to the hand that pressed it, and a mark that lands only in one
  // of the two is a key that stops working halfway through an evening.
  const key = ev.key.length === 1 ? ev.key.toLowerCase() : ev.key;
  if (playing && key !== "i" && key !== "o" && key !== "k") stopPlay();
  const step = ev.shiftKey ? 1 : frame();
  if (ev.key === "ArrowRight" || ev.key === "ArrowLeft") {
    ev.preventDefault();
    if (arrowDue(ev)) scrubTo(playOut() + (ev.key === "ArrowRight" ? step : -step));
    return;
  }
  // Up and down walk the access points -- the pictures a cut is free at.
  // Held down they are the same flood the left and right keys are, so they
  // go through the same gate.
  if (ev.key === "ArrowUp" || ev.key === "ArrowDown") {
    ev.preventDefault();
    if (arrowDue(ev)) scrubTo(srcToOutSeam(nearestPoint(playhead, ev.key === "ArrowDown" ? 1 : -1)));
    return;
  }
  if (key === "i") setIn(playOut());
  if (key === "o") setOut(playOut());
  if (key === "k") addKeyframes([playhead], playhead);
  if (key === "s") toScene(ev.shiftKey ? -1 : 1);
});

// --- commercial breaks --------------------------------------------------

// --- 書き出すトラック ------------------------------------------------------
//
// A broadcast recording is more than a picture and a sound. It carries the
// captions, sometimes a second language, and things a cut cannot take with
// it at all. Which of them are written is a decision about this clip, so it
// is made here and travels back to the list inside the edit.
//
// The default is everything, and that is deliberate: a track nobody asked
// about is a track that was in the recording, and dropping it silently would
// be this program deciding what the recording is for.

/// Source stream indices switched off. Empty is the ordinary case.
let dropStreams = [];
/// What the backend last said this recording carries, or null before it has
/// been asked. Kept so reopening the menu does not ask again.
let trackList = null;

/// Say on the button whether anything is being left out.
///
/// Only when something is: a label that reads "0 left out" is a label that
/// makes the ordinary case look like a decision.
function paintTrackButton() {
  const b = el("tracks");
  if (!b) return;
  b.textContent = dropStreams.length
    ? `${tr("tracks.button")} (${tr("tracks.summary", { n: dropStreams.length })})`
    : tr("tracks.button");
}

function renderTracks() {
  const list = el("tracks-list");
  list.innerHTML = "";
  if (!trackList || !trackList.length) {
    const li = document.createElement("li");
    li.className = "dim";
    li.textContent = tr("tracks.none");
    list.appendChild(li);
    return;
  }
  // Listed to be honest about them, not to be chosen between, so they go
  // under the tracks that are a choice rather than among them -- with the
  // reason said once at the end and not after each.
  const dropped = trackList.filter(
    (k) => !k.optional && k.kind !== "subpicture" && k.detail !== "data",
  );
  // The data broadcast is carried, and not by a choice made here: it is one
  // answer for the whole run, on the output settings screen, because only a
  // `.ts` can hold one. Listing it as "not carried" was true before that
  // screen had the question and is not true now.
  const settled = trackList.filter((k) => !k.optional && k.detail === "data");
  // A DVD's subtitles are not a choice made here and are not dropped either:
  // they travel beside the cut. Said on their own line, because either of the
  // lists they would otherwise land in would be saying something untrue.
  const beside = trackList.filter((k) => k.kind === "subpicture");
  for (const track of trackList) {
    if (!track.optional) continue;
    const li = document.createElement("li");
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = !dropStreams.includes(track.index);
    box.id = `track-${track.index}`;
    box.addEventListener("change", () => {
      dropStreams = box.checked
        ? dropStreams.filter((i) => i !== track.index)
        : dropStreams.concat([track.index]);
      paintTrackButton();
      // Switching the wider track off makes the narrower one the track the
      // line speaks for.
      paintSourceInfo();
      sync();
    });
    const label = document.createElement("label");
    label.htmlFor = box.id;
    const kinds = {
      audio: "tracks.audio",
      graphics: "tracks.graphics",
      superimpose: "tracks.superimpose",
    };
    const kind = tr(kinds[track.kind] || "tracks.caption");
    const bits = [kind, track.detail];
    if (track.language) bits.push(track.language);
    if (track.main) bits.push(tr("tracks.main"));
    bits.push(tr("tracks.pid", { pid: track.pid.toString(16).padStart(4, "0") }));
    label.textContent = bits.join(tr("sep"));
    li.appendChild(box);
    li.appendChild(label);
    list.appendChild(li);
  }
  /// What the engine calls each kind, and what this window calls it back.
  ///
  /// A crawl is named twice over, and not with the same words: the list
  /// above calls a track by what it is, and this line finishes the sentence
  /// "not carried: ...". Two names for the one kind, which is why the second
  /// is `tracks.gone.superimpose` -- while both were `tracks.superimpose`
  /// the catalogue held the name twice and the later one won, so the English
  /// list said "superimposed text" where it meant "Crawl".
  const named = {
    superimpose: "tracks.gone.superimpose",
    data: "tracks.data",
    substream: "tracks.substream",
    menu: "tracks.menu",
    "text subtitles": "tracks.textst",
  };
  for (const track of dropped) {
    const li = document.createElement("li");
    li.className = "dim";
    li.textContent = tr("tracks.dropped", {
      what: tr(named[track.detail] || "tracks.data"),
      pid: track.pid.toString(16).padStart(4, "0"),
    });
    list.appendChild(li);
  }
  for (const track of settled) {
    const li = document.createElement("li");
    li.className = "dim";
    li.textContent = tr("tracks.settled", {
      what: tr("tracks.data"),
      pid: track.pid.toString(16).padStart(4, "0"),
    });
    list.appendChild(li);
  }
  for (const track of beside) {
    const li = document.createElement("li");
    li.className = "dim";
    li.textContent = tr("tracks.beside", {
      lang: track.language ? `${track.language} ` : "",
      pid: track.pid.toString(16).padStart(2, "0"),
    });
    list.appendChild(li);
  }
  // Three reasons, each said only where it applies: what cannot go on a cut
  // timeline at all, what could have but has nowhere to be written, and what
  // travels by an answer given on another screen.
  for (const [key, when, from] of [
    ["tracks.droppedNote", (k) => k.detail !== "substream", dropped],
    ["tracks.substreamNote", (k) => k.detail === "substream", dropped],
    ["tracks.settledNote", () => true, settled],
  ]) {
    if (!from.some(when)) continue;
    const note = document.createElement("li");
    note.className = "dim small";
    note.textContent = tr(key);
    list.appendChild(note);
  }
  // The other half of the answer, and the half nobody would think to ask
  // for: the programme information is kept, and it is not a track.
  const tables = document.createElement("li");
  tables.className = "dim small";
  tables.textContent = tr("tracks.tablesNote");
  list.appendChild(tables);
}

el("tracks").addEventListener("click", async () => {
  if (!src) return;
  el("tracks-modal").hidden = false;
  if (!trackList) {
    try {
      trackList = await invoke("tracks", { path: src.path });
    } catch (e) {
      el("status").textContent = tr("tracks.failed", { e });
      el("tracks-modal").hidden = true;
      return;
    }
  }
  renderTracks();
});

el("tracks-close").addEventListener("click", () => {
  el("tracks-modal").hidden = true;
});

// The ground behind the panel, which is the other way out of one.
el("tracks-modal").addEventListener("mousedown", (ev) => {
  if (ev.target === el("tracks-modal")) el("tracks-modal").hidden = true;
});

el("detect-cm").addEventListener("click", async () => {
  if (!src) return;
  el("detect-cm").disabled = true;
  showCmNote(tr("editor.detecting"));
  try {
    const res = await invoke("detect_cm", { path: src.path });
    cmSummary = cmNote(res);
    showCmNote(cmSummary);
    applyCmBlocks(res.blocks);
    // Reported to the clip list too, so the row says what was found and a
    // later visit to this clip does not have to detect it again.
    sync();
  } catch (e) {
    showCmNote(tr("cm.failed", { e }));
  } finally {
    el("detect-cm").disabled = false;
    el("detect-cm").textContent = tr("editor.detectCm");
  }
});

if (listen) {
  listen("play-ended", (ev) => {
    // The end of a run that has already been left behind -- a ◀◀ stopped it
    // and asked for another from ten seconds back -- is not the end of what
    // is playing now.
    if (ev.payload !== playRun || !playing) return;
    setPlaying(false);
    // Round again from where this one started. Only ever reached by playback
    // running out of ranges: a 停止 has already put `playing` down, so the
    // one that arrives after it falls out above.
    if (looping && loopAt !== null) {
      movePlayhead(loopAt);
      startPlay();
      return;
    }
    showFrame(playhead);
  });
  // The video half of playback has no way to notice the audio half failed --
  // they run on separate threads and separate clocks -- so without this the
  // picture just plays silently with nothing on screen to say why.
  listen("audio-error", (ev) => {
    el("status").textContent = tr("editor.audioFailed", { e: ev.payload });
  });
  listen("cm-progress", (ev) => {
    const [phase, done] = ev.payload;
    el("detect-cm").textContent = tr("editor.detectingPct", { pct: Math.round(done * 100) });
    showCmNote(phase);
  });
  listen("prepare-progress", (ev) => {
    const [phase, done] = ev.payload;
    if (!warmed) showWarm(tr("warm.progress", { phase, pct: Math.round(done * 100) }));
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
}

window.addEventListener("resize", relayout);

// --- talking to the list window ------------------------------------------
//
// Two documents, so nothing is shared but events. The list says which clip
// and hands over whatever was done to it last time; the editor says what has
// been done to it since, as it happens.
//
// Reporting continuously rather than only at OK is what makes the title
// bar's cross safe: by the time the window goes away the list already has
// everything, so closing it that way is the same as OK. キャンセル is the
// one that needs saying out loud, because it means "put back what you had".

/// Redraw everything that was measured in pixels.
///
/// The canvas and the film strip are both laid out against the width they
/// are actually given, so a resized window is a wrong offset and a wrong
/// number of cells until this runs.
function relayout() {
  draw();
  drawSubs();
  // the reel is placed in pixels, so a narrower window is a wrong offset --
  // and a narrower window holds fewer cells, so it wants a fresh reel too
  holdReel();
  scheduleStrip();
}

/// Everything this session has done to the recording, in source time.
function captureEdit() {
  if (!src) return null;
  return {
    id: editId,
    path: src.path,
    cuts: cuts.map((c) => ({ a: c.a, b: c.b })),
    keyframes: keyframes.slice(),
    activeKey,
    cmBlocks,
    cmNote: cmSummary,
    // Streams the track menu switched off, by source stream index. Part of
    // the edit because it is about this clip and nothing else: the same
    // recording can be in the list twice, one copy with the dub and one
    // without, and the output settings are one answer for the whole list.
    dropStreams: dropStreams.slice(),
    playhead,
    selA,
    selB,
  };
}

/// Tell the list what the timeline looks like now.
///
/// Coalesced: a cut moves the marks, the plan, the strip and the scrubber,
/// and each of those would otherwise report the same state again.
/// Which row of the list is being cut. Not the path: the same recording can
/// be in the list twice, cut two different ways, and what comes back out of
/// here has to land on the row it came from.
let editId = null;
/// The row being loaded, if one is. See the `editor-open` handler.
let opening = null;

let syncTimer = null;
function sync() {
  clearTimeout(syncTimer);
  syncTimer = setTimeout(() => {
    const state = captureEdit();
    if (state && emit) emit("editor-state", state);
  }, 150);
}

/// Put the marks a commercial detection found onto the timeline.
///
/// A block's start is where the commercials begin and its end is where the
/// programme comes back, so both are worth a mark -- along with the opening
/// of the recording itself, which is the head of the material and not of the
/// clock: nothing before the first access point can be decoded.
///
/// Not snapped to an access point: the mark should say where the cut
/// actually is, to the frame. Moving it onto the nearest lossless point is a
/// separate decision, and there is a button for it.
function applyCmBlocks(blocks) {
  if (!src || !blocks || !blocks.length) return;
  cmBlocks = blocks;
  addKeyframes([headTime()].concat(blocks.flatMap((b) => [b.start, b.end])));
  draw();
}

el("editor-ok").addEventListener("click", () => {
  sync();
  // After the debounce, so the last cut is over the wire before the window
  // that made it goes away.
  setTimeout(() => invoke("close_editor"), 220);
});

/// Leave without what was done in here. The list is told to drop it -- `sync`
/// has been reporting every cut as it happened -- and the window goes. Both
/// the button and Escape.
function cancelEdit() {
  clearTimeout(syncTimer);
  if (emit) emit("editor-cancel", editId);
  setTimeout(() => invoke("close_editor"), 80);
}

el("editor-cancel").addEventListener("click", cancelEdit);

if (listen) {
  // The list's answer to `editor-ready`: which recording, and what was done
  // to it the last time it was in here.
  listen("editor-open", async (ev) => {
    const { id, path, name, side, saved, cm, chapters, dropPids } = ev.payload;
    // The list sends this twice for a window it had to build; the second is
    // the one that usually lands, but both can. Opening the same row twice
    // over would throw away whatever the first open had got to.
    if (opening === id) return;
    // Reloaded when the *row* changes, not merely the recording: two rows can
    // be the same file cut two different ways, and coming from one to the
    // other has to bring the other one's cuts with it.
    // Whether this open is what put the recording up, which decides whether
    // the blocks below are part of it arriving or something done to it.
    const arriving = editId !== id;
    if (arriving) {
      opening = id;
      editId = id;
      try {
        await openPath(path, saved, side, name, chapters, dropPids);
      } finally {
        opening = null;
      }
    }
    // Blocks the list found on its own, which only this window can turn into
    // marks: it is the one that knows where the material begins. Applied
    // whether or not the recording was already up -- a detection run from the
    // list while this window sat open on the same clip has marks to put down
    // just the same. That one is an edit and steps back like any other; marks
    // a recording comes up with are not, and `settle` keeps them out of the
    // history.
    if (cm && cm.blocks && cm.blocks.length) {
      if (arriving) await settle(() => applyCmBlocks(cm.blocks));
      else applyCmBlocks(cm.blocks);
      cmSummary = cm.note || "";
      showCmNote(cmSummary);
    }
    relayout();
    sync();
  });

  // The list window is where 環境設定 lives, so a language change is news
  // that arrives from there. It carries the language it settled on rather
  // than the preference, because "follow the machine" is answered once, in
  // that window, and both windows have to land on the same answer.
  listen("lang-changed", (ev) => setLang(ev.payload, false));
  // 環境設定 is in the other window, and this one has its own copy of
  // everything the store holds. The counter is the half that can be applied
  // where it stands; which subtitle track to start with is answered when a
  // recording is opened, so a window already up keeps the one it has.
  listen("prefs-changed", (ev) => {
    const said = ev.payload || {};
    if (typeof said.counter === "boolean") showCounter(said.counter, false);
  });
  // A row renamed in the list while this window is up. The name is the list's
  // to give -- it is the row that was renamed and not the recording -- so it
  // arrives here rather than being worked out again, and only for the row this
  // window is actually on.
  listen("clip-renamed", (ev) => {
    const said = ev.payload || {};
    if (said.id !== editId) return;
    shownName = said.name || null;
    paintSourceInfo();
  });
}

/// Everything this window has drawn out of the catalogue since it opened.
///
/// The static markup is `applyStatic`'s and has already been done. What is
/// left is the readouts, which are all cheap to draw again -- except the
/// plan, which is a call into the engine, and the one line this window does
/// not own: the detection's summary came over the wire already written, and
/// re-wording it here would mean holding what it was made of.
onLangChange(() => {
  el("detect-cm").textContent = tr("editor.detectCm");
  // The label carries a count when something is left out, so `applyStatic`
  // has just written the plain word over it.
  paintTrackButton();
  if (!el("tracks-modal").hidden) renderTracks();
  el("play").textContent = tr(playing ? "t.stop" : "t.play");
  paintSourceInfo();
  if (src) {
    updateReadouts();
    renderKeyframes();
    showFrame(playhead);
    schedulePlan();
  }
});

noBrowserMenu();
noNativeDrag();
applyStatic();
// The two labels that say what would happen rather than what the button is,
// and are therefore written from here rather than by `applyStatic`.
el("detect-cm").textContent = tr("editor.detectCm");
el("play").textContent = tr("t.play");
renderKeyframes();
draw();
jlog("editor wired");
// The window is up and has nothing in it; the list is what fills it.
if (emit) emit("editor-ready", null);
// A second opinion on what the machine is set to, for a window opened before
// the list window had a chance to pass its own on.
confirmWithOs(invoke);
