// 継ぎ目の編集: one join out of the list, set and looked at.
//
// The window is built in Rust (`open_cross`) and filled over the wire, the
// same handshake the cut editor makes: this says `cross-ready` when its page
// is up, the list answers with `cross-open` carrying every join in the list
// and which one to start on, and OK sends the settings back as `cross-done`.
// キャンセル sends nothing, which is what makes it a cancel.
//
// **Nothing is decided here about what a transition *is*.** The effect, its
// length, its easing and the image over it are written into the list's own
// clips and are what the cutter is given; this window is a way of choosing
// them with the result on screen. What it does own is the preview: which
// instant of the seam is being shown, and whether it is playing.
//
// The picture comes from the engine already composited (`cross_shot`,
// `cross_play`). It could not be done in here -- a dissolve is arithmetic
// over two decoded frames, and a webview has neither of them -- and it should
// not be: the preview and the output have to agree, and the way to make two
// things agree is for there to be one of them. See `crossview.rs`.

const T = window.__TAURI__ || {};
const invoke = T.core && T.core.invoke;
const listen = T.event && T.event.listen;
const emit = T.event && T.event.emit;
const jlog = (m) => invoke && invoke("log", { msg: String(m) });

window.addEventListener("error", (e) => jlog(`cross error ${e.message}`));
window.addEventListener("unhandledrejection", (e) => jlog(`cross reject ${e.reason}`));

import { fmt, noBrowserMenu, noNativeDrag, wireDrops, MENU_MARGIN, MENU_LEAST } from "./shared.js";
import { t as tr, applyStatic, onLangChange, setLang } from "./i18n.js";
import * as prefs from "./prefs.js";

const el = (id) => document.getElementById(id);
const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));

/// Every join in the list, as the list stated them. Each carries the two
/// clips it sits between, what is kept of each at this seam, and the
/// transition itself -- which is the only part this window writes to.
let joins = [];
/// Which one is being looked at, by index into `joins`.
let at = 0;
/// What the engine said about the two recordings of the join in hand.
let facts = null;
/// The previewed stretch: how long it runs and where the crossing sits in it.
let span = null;
/// Where the playhead is, on the preview's own clock.
let head = 0;
/// The instant the picture on the stage is of, which is not always the
/// playhead: a still arrives a moment after it was asked for.
let shown = 0;
let playing = false;
let looping = false;
/// Names each run of playback, so a picture from a run that has been stopped
/// can be told from one of the run now going. See `Playing` on the Rust side.
let playRun = 0;
/// The instant of the newest picture shown, so an older one arriving late is
/// not put back on the stage.
let playAt = -Infinity;
let playUrl = null;
/// Set while the list has not answered yet. See `announceReady`.
let answered = false;

/// Counts which join and which setting are the latest asked about, so that
/// an answer overtaken by a later one is dropped rather than drawn. See
/// `showJoin` and `refreshSpan`.
let joinRun = 0;
let spanRun = 0;

/// The loads of a join's two recordings, one after another. The backend
/// holds one pair at a time, and two loads side by side finished in either
/// order: the slower, older one left the pair behind and every picture after
/// it came out of the recordings of the join before.
let loading = Promise.resolve();

/// Whether anything in this window has been changed since the list sent it.
let changedHere = false;

const wiring = [];
const hear = (name, fn) => wiring.push(listen(name, fn));

/// The join in hand, or null before the list has said anything.
const join = () => joins[at] || null;

/// Its transition, made if this is the first thing said about this join.
///
/// The list sends a join with no transition on it as `null`, because a list
/// is mostly joins nobody has said anything about and an object on each of
/// them would be twenty objects saying "nothing happens here". One is made
/// here on the first look, and `cross-done` sends back whatever is on the
/// join when OK is pressed.
function crossing() {
  const j = join();
  if (!j) return null;
  if (!j.after) {
    j.after = {
      kind: "none",
      seconds: 1,
      curve: "none",
      mode: "in",
      image: "",
      fadeOut: 0,
      fadeIn: 0,
    };
  }
  return j.after;
}

// --- what is on screen ---------------------------------------------------

/// The stage's width in real pixels, which is what a picture is asked for.
function stageWidth() {
  const box = el("preview").parentElement.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  return Math.max(160, Math.round(box.width * dpr));
}

/// The rate the preview is walked at. The clip before the join's own, because
/// that is the rate the joined file is written at where it is the master.
const fps = () => (facts && facts.fps > 0 ? facts.fps : 30);

/// Which clip the instant `t` belongs to, for the line on the picture.
function whereAt(t) {
  if (!span) return "xw.beforeClip";
  if (t < span.at - 1e-9) return "xw.beforeClip";
  if (t >= span.ends - 1e-9) return "xw.afterClip";
  return "xw.crossing";
}

function updateReadouts() {
  const total = span ? span.seconds : 0;
  const n = Math.round(shown * fps());
  const of = Math.max(0, Math.round(total * fps()));
  el("ovl-frame").textContent = `${n} / ${of}`;
  el("ovl-time").textContent = fmt(shown);
  el("ovl-where").textContent = tr(whereAt(shown));
  // What the two clips come to once the crossing has taken what it takes,
  // and what the crossing itself runs for. The second is not always what was
  // asked for: a transition longer than the material either side of the join
  // is held to what there is. See `Seam::takes`.
  const runs = span ? Math.max(0, span.ends - span.at) : 0;
  el("out-clip-time").textContent = fmt(total);
  el("out-cross-time").textContent = fmt(runs);
}

/// Draw the scrubber: the previewed stretch, with the crossing marked in it
/// and the playhead over the lot.
function drawScrub() {
  const face = el("xscrub");
  const box = face.parentElement.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  const w = Math.max(1, Math.round(box.width * dpr));
  const h = Math.max(1, Math.round(26 * dpr));
  if (face.width !== w || face.height !== h) {
    face.width = w;
    face.height = h;
  }
  const g = face.getContext("2d");
  g.clearRect(0, 0, w, h);
  const total = span && span.seconds > 0 ? span.seconds : 1;
  const x = (t) => Math.round((clamp(t, 0, total) / total) * (w - 1));
  // The bar itself, in two shades: what belongs to the clip before the join
  // and what belongs to the clip after. A bare bar would say nothing about
  // where the crossing falls between the two, which is the whole question.
  g.fillStyle = "#2f2f2f";
  g.fillRect(0, 0, w, h);
  if (span) {
    g.fillStyle = "#3d4a52";
    g.fillRect(0, 0, x(span.at), h);
    g.fillStyle = "#4a3d52";
    g.fillRect(x(span.ends), 0, w - x(span.ends), h);
    // And the crossing between them, where both are on screen.
    if (span.ends > span.at + 1e-9) {
      g.fillStyle = "#c98a2e";
      g.fillRect(x(span.at), 0, Math.max(2, x(span.ends) - x(span.at)), h);
    } else {
      // A plain cut: a line rather than a stretch, because it takes no time.
      g.fillStyle = "#8a8a8a";
      g.fillRect(x(span.at), 0, Math.max(1, Math.round(dpr)), h);
    }
  }
  const px = x(head);
  g.fillStyle = "#f0f0f0";
  g.fillRect(Math.max(0, px - Math.round(dpr)), 0, Math.max(2, Math.round(2 * dpr)), h);
}

/// Draw what the effect does, halfway through it.
///
/// Two blocks standing for the two clips, put together the way the chosen
/// effect puts them. Halfway, because that is where both are visible whatever
/// the effect is -- and drawn rather than named, since which way a wipe
/// travels and what a slide does to the picture behind it are things a name
/// only half says.
function drawPattern() {
  const face = el("pattern");
  const g = face.getContext("2d");
  const [w, h] = [face.width, face.height];
  const kind = crossing() ? crossing().kind : "none";
  const label = (text, x, y, colour) => {
    g.fillStyle = colour;
    g.font = `bold ${Math.round(h / 3)}px sans-serif`;
    g.textAlign = "center";
    g.textBaseline = "middle";
    g.fillText(text, x, y);
  };
  const A = "#2f6f7f";
  const B = "#7f5a2f";
  g.clearRect(0, 0, w, h);
  if (kind === "none") {
    g.fillStyle = A;
    g.fillRect(0, 0, w / 2, h);
    g.fillStyle = B;
    g.fillRect(w / 2, 0, w / 2, h);
    label("A", w / 4, h / 2, "#fff");
    label("B", (3 * w) / 4, h / 2, "#fff");
    return;
  }
  if (kind === "fade-black" || kind === "fade-white") {
    // Halfway through a fade is the colour itself, which is the whole of what
    // there is to show: neither clip is on screen at that instant.
    g.fillStyle = kind === "fade-white" ? "#e8e8e8" : "#0a0a0a";
    g.fillRect(0, 0, w, h);
    label("A → B", w / 2, h / 2, kind === "fade-white" ? "#333" : "#ccc");
    return;
  }
  if (kind === "dissolve") {
    g.fillStyle = A;
    g.fillRect(0, 0, w, h);
    g.globalAlpha = 0.5;
    g.fillStyle = B;
    g.fillRect(0, 0, w, h);
    g.globalAlpha = 1;
    label("A + B", w / 2, h / 2, "#fff");
    return;
  }
  // A wipe and a slide differ in what the clip behind the edge does: a wipe
  // leaves it standing, a slide pushes it off. Drawn as the edge halfway
  // across, with the outgoing block shifted for a slide.
  const [, side] = kind.split("-");
  const moving = kind.startsWith("slide");
  const vertical = side === "top" || side === "bottom";
  const along = vertical ? h : w;
  const cut = along / 2;
  const place = (fill, from, to) => {
    g.fillStyle = fill;
    if (vertical) g.fillRect(0, from, w, to - from);
    else g.fillRect(from, 0, to - from, h);
  };
  const near = side === "left" || side === "top";
  // The clip being left: still where it was for a wipe, pushed away for a
  // slide.
  const shove = moving ? cut : 0;
  place(A, near ? shove : -shove, (near ? shove : -shove) + along);
  place(B, near ? 0 : cut, near ? cut : along);
  label("A", vertical ? w / 2 : near ? (3 * along) / 4 : along / 4,
        vertical ? (near ? (3 * along) / 4 : along / 4) : h / 2, "#fff");
  label("B", vertical ? w / 2 : near ? along / 4 : (3 * along) / 4,
        vertical ? (near ? along / 4 : (3 * along) / 4) : h / 2, "#fff");
}

// --- asking the engine ---------------------------------------------------

/// The join in hand in the shape the engine reads.
function seamSpec() {
  const j = join();
  if (!j) return null;
  const c = crossing();
  return {
    beforeIn: j.beforeIn,
    beforeOut: j.beforeOut,
    afterIn: j.afterIn,
    afterOut: j.afterOut,
    crossing: {
      kind: c.kind,
      seconds: Number(c.seconds) || 0,
      curve: c.curve || "none",
      mode: c.mode || "in",
      image: c.image || null,
      // The sound, which the preview plays: the clip before fades out into
      // the handover and the clip after fades in from it, over the same
      // curve the cut writes. A fade that could only be heard in the
      // finished file would be a setting made blind, which is the one thing
      // this window exists not to be.
      fadeOut: Number(c.fadeOut) || 0,
      fadeIn: Number(c.fadeIn) || 0,
    },
  };
}

/// Work out the previewed stretch again, and redraw everything that depends
/// on it. Called on every change of a setting.
async function refreshSpan() {
  const spec = seamSpec();
  if (!spec || !invoke) return;
  const run = ++spanRun;
  let got;
  try {
    got = await invoke("cross_span", { seam: spec });
  } catch (e) {
    jlog(`cross_span: ${e}`);
    return;
  }
  // A slider dragged asks on every step, and the answers need not come back
  // in order: only the last question's is the setting on screen.
  if (run !== spanRun) return;
  span = got;
  head = clamp(head, 0, span.seconds);
  drawScrub();
  updateReadouts();
  paintNote();
}

/// Whether a still is already on its way, so that a pointer dragged across
/// the scrubber does not queue one request per pixel: each answer asks for
/// the instant the pointer had reached by the time it landed, and a drag
/// therefore costs about as many decodes as the machine can manage rather
/// than as many as the mouse can report.
let shotBusy = false;
let shotWanted = null;

async function showFrame(t) {
  if (!invoke || playing) return;
  shotWanted = t;
  if (shotBusy) return;
  shotBusy = true;
  try {
    while (shotWanted !== null) {
      const want = shotWanted;
      shotWanted = null;
      const spec = seamSpec();
      if (!spec) break;
      const shot = await invoke("cross_shot", {
        seam: spec,
        time: want,
        width: Math.min(stageWidth(), 1280),
      }).catch((e) => {
        jlog(`cross_shot: ${e}`);
        return null;
      });
      if (!shot || playing) break;
      el("preview").src = shot.url;
      shown = want;
      updateReadouts();
    }
  } finally {
    shotBusy = false;
  }
}

function showPlayFrame(run, buf) {
  if (run !== playRun || !playing || !buf || buf.byteLength <= 8) return;
  const t = new DataView(buf).getFloat64(0, true);
  // Two fetches can finish in the other order; a picture older than the one
  // on the stage is one nobody wants back.
  if (t < playAt) return;
  playAt = t;
  const was = playUrl;
  playUrl = URL.createObjectURL(new Blob([new Uint8Array(buf, 8)], { type: "image/jpeg" }));
  el("preview").src = playUrl;
  if (was) URL.revokeObjectURL(was);
  head = t;
  shown = t;
  updateReadouts();
  drawScrub();
}

function setPlaying(on) {
  playing = on;
  el("play").textContent = tr(on ? "t.stop" : "t.play");
  el("play").classList.toggle("on", on);
}

function startPlay() {
  if (!invoke || playing || !span || span.seconds <= 0) return;
  const spec = seamSpec();
  if (!spec) return;
  // From the top again where the playhead is sitting at the end: pressing 再生
  // on a preview that has just finished means "again", not "nothing".
  if (head >= span.seconds - 1e-3) head = 0;
  setPlaying(true);
  playAt = -Infinity;
  const run = ++playRun;
  const frames = new T.core.Channel();
  frames.onmessage = (buf) => showPlayFrame(run, buf);
  // not awaited: it resolves when playback ends, and `cross-play-ended` says
  // so -- the same arrangement the cut editor's own playback has.
  invoke("cross_play", {
    seam: spec,
    from: head,
    width: Math.min(stageWidth(), 1280),
    fps: fps(),
    run,
    frames,
  }).catch((e) => {
    el("note").textContent = tr("editor.playFailed", { e });
    setPlaying(false);
  });
}

function stopPlay(repaint = true) {
  if (!playing) return;
  invoke && invoke("stop_play", { cross: true });
  setPlaying(false);
  if (repaint) showFrame(head);
}

// --- the join in hand ----------------------------------------------------

/// Put a join's settings into the controls, and open its two recordings.
/// The line in the window and the window's own title bar say the same
/// thing, and both move with the picker: a title bar left naming the join
/// before this one is a window that says it is about something it is not.
function nameWindow(j) {
  const named = tr("xw.windowTitle", {
    before: j.beforeName || "",
    after: j.afterName || "",
  });
  el("title").textContent = named;
  invoke && invoke("retitle_cross", { title: named }).catch(() => {});
}

async function showJoin() {
  const j = join();
  if (!j) return;
  const c = crossing();
  el("x-kind").value = c.kind;
  if (el("x-kind").selectedIndex < 0) el("x-kind").selectedIndex = 0;
  showSecs(c.seconds);
  el("x-curve").value = c.curve || "none";
  el("x-mode").value = c.mode || "in";
  el("x-image").value = c.image || "";
  // The sound, which is not greyed with the rest: a fade under no crossing
  // at all is an ordinary thing to ask for.
  el("x-fade-out").value = fadeSecs(c.fadeOut);
  el("x-fade-in").value = fadeSecs(c.fadeIn);
  el("name-before").textContent = j.beforeName || "";
  el("name-after").textContent = j.afterName || "";
  el("pic-before").src = j.beforePic || "";
  el("pic-after").src = j.afterPic || "";
  nameWindow(j);
  drawPattern();
  paintLive();
  head = 0;
  span = null;
  updateReadouts();
  if (!invoke) return;
  el("note").textContent = tr("xw.reading");
  const run = ++joinRun;
  const load = loading.then(() =>
    invoke("cross_load", { before: j.beforePath, after: j.afterPath }),
  );
  loading = load.catch(() => {});
  let got;
  try {
    got = await load;
  } catch (e) {
    if (run === joinRun) el("note").textContent = tr("xw.cannotRead", { e });
    return;
  }
  // Another join was picked while this one loaded; its own load comes after
  // this one and is the pair the backend ends up holding.
  if (run !== joinRun) return;
  facts = got;
  el("note").textContent = "";
  // The sound is only offered where one of the two has any.
  const heard = facts.beforeAudio || facts.afterAudio;
  for (const id of ["mute", "volume"]) el(id).disabled = !heard;
  await refreshSpan();
  showFrame(0);
}

/// Grey the rows that describe a crossing there is not one of.
///
/// Greyed rather than hidden: they sit in a column of fixed rows, and
/// controls coming and going would move the ones below them under the hand.
function paintLive() {
  const on = crossing() && crossing().kind !== "none";
  for (const id of ["x-slider", "x-secs", "x-curve", "x-mode", "x-image", "x-browse"]) {
    el(id).disabled = !on;
  }
}

/// What the setting costs the output, in the window's own words.
function paintNote() {
  const c = crossing();
  if (!c || c.kind === "none" || !span) {
    el("note").textContent = "";
    return;
  }
  const runs = Math.max(0, span.ends - span.at);
  // A fade takes half from each side and the output keeps its length; every
  // other kind shows both clips at once and the output comes out shorter by
  // what they share. The same two sentences the settings screen used to
  // print, which is where they came from.
  const overlaps = !c.kind.startsWith("fade") && c.kind !== "none";
  el("note").textContent = overlaps
    ? tr("outset.crossShortens", { secs: runs.toFixed(1) })
    : tr("outset.crossKeeps");
}

/// Take a setting from a control and put it on the join in hand.
function take(what, value) {
  const c = crossing();
  if (!c) return;
  c[what] = value;
  changedHere = true;
  if (what === "kind") {
    drawPattern();
    paintLive();
  }
  refreshSpan().then(() => {
    if (!playing) showFrame(head);
  });
}

// --- the controls --------------------------------------------------------

el("x-kind").addEventListener("change", () => take("kind", el("x-kind").value));
el("x-curve").addEventListener("change", () => take("curve", el("x-curve").value));
el("x-mode").addEventListener("change", () => take("mode", el("x-mode").value));

/// The slider and the field are two views of one number, so each writes the
/// other -- and the bar behind the handle is filled to where it stands, the
/// way the volume's is. The track is drawn by us (see `.vslider`) and it is
/// `--at` that tells it where to stop.
function showSecs(v) {
  const secs = clamp(Number(v) || 0.1, 0.1, 30);
  el("x-slider").value = secs;
  el("x-secs").value = secs;
  el("x-slider").style.setProperty("--at", `${((secs - 0.1) / 29.9) * 100}%`);
}

/// `input` rather than `change` on the slider: what it is for is finding a
/// length by eye, and a length that only arrived when the hand let go could
/// not be found that way.
el("x-slider").addEventListener("input", () => {
  const v = Number(el("x-slider").value);
  showSecs(v);
  take("seconds", v);
});
el("x-secs").addEventListener("change", () => {
  const v = clamp(Number(el("x-secs").value) || 0.1, 0.1, 30);
  showSecs(v);
  take("seconds", v);
});
el("x-image").addEventListener("change", () => take("image", el("x-image").value.trim()));

/// A fade length as the field holds one: tenths of a second, none to ten.
///
/// Ten seconds is the ceiling the command line puts on the fade at a cut,
/// and it is the same answer for the same reason -- past it the fade is not
/// a join being smoothed, it is the programme being turned down.
const fadeSecs = (v) => Math.round(clamp(Number(v) || 0, 0, 10) * 10) / 10;

for (const [id, what] of [
  ["x-fade-out", "fadeOut"],
  ["x-fade-in", "fadeIn"],
]) {
  el(id).addEventListener("change", () => {
    const secs = fadeSecs(el(id).value);
    el(id).value = secs;
    take(what, secs);
  });
}

el("x-browse").addEventListener("click", async () => {
  if (!T.dialog) return;
  const picked = await T.dialog
    .open({
      multiple: false,
      filters: [{ name: tr("outset.crossImageKind"), extensions: ["png", "jpg", "jpeg", "bmp", "gif", "webp"] }],
    })
    .catch(() => null);
  if (!picked) return;
  el("x-image").value = picked;
  take("image", picked);
});

// --- which join ----------------------------------------------------------
//
// A control of our own rather than a `<select>`: what tells one join from
// another is the two recordings it sits between, and a `<select>` holds
// nothing but text. The face is the pair in hand; the list under it is every
// join drawn the same way. The main window draws its own popups too, for a
// different reason -- see `opensUpward` there -- and the two share the menu's
// look and its keys so that a list that drops in this window behaves like a
// list that drops in that one.

/// The two recordings a join sits between, drawn.
///
/// The same shape as the face in the markup, built here because the list is
/// as many of these as there are joins and the face is one of them. The
/// pictures are what the list handed over -- see `cross-open` -- so this
/// costs nothing but the nodes.
function pairNode(j) {
  const box = document.createElement("div");
  box.className = "pair";
  for (const side of ["before", "after"]) {
    const row = document.createElement("div");
    row.className = "pair-row";
    if (side === "after") {
      const arrow = document.createElement("span");
      arrow.className = "pair-arrow";
      arrow.textContent = "┗▶";
      row.appendChild(arrow);
    }
    const pic = document.createElement("img");
    pic.className = "pair-pic";
    pic.alt = "";
    pic.src = j[`${side}Pic`] || "";
    const name = document.createElement("span");
    name.className = "pair-name";
    name.textContent = j[`${side}Name`] || "";
    row.append(pic, name);
    box.appendChild(row);
  }
  return box;
}

/// The picker: the face, the list it drops, and the keys both answer to.
function pairPicker() {
  const face = el("which-join");
  const menu = el("join-menu");
  let items = [];
  /// Where the cursor is, which the mouse and the arrow keys both move. -1
  /// while the list is down.
  let cursor = -1;

  const open = () => {
    if (!joins.length) return;
    menu.innerHTML = "";
    items = joins.map((j, i) => {
      const li = document.createElement("li");
      li.setAttribute("role", "option");
      // What a reader hears. The row itself is two pictures and two names,
      // and a picture says nothing to a reader; this is the sentence the
      // settings screen used to write on the row instead.
      li.setAttribute("aria-label", tr("outset.crossAfter", { n: i + 1, name: j.beforeName || "" }));
      if (i === at) li.className = "on";
      li.appendChild(pairNode(j));
      menu.appendChild(li);
      return li;
    });
    cursor = at;
    menu.hidden = false;
    face.setAttribute("aria-expanded", "true");
    // Which side of the control there is room on. This panel is at the top
    // of the column, so the answer is usually below it -- but the window can
    // be dragged short, and a list that runs off the bottom is a list with
    // its last episodes out of reach.
    const box = face.getBoundingClientRect();
    const below = window.innerHeight - box.bottom - MENU_MARGIN;
    const above = box.top - MENU_MARGIN;
    const down = below > above;
    menu.classList.toggle("down", down);
    menu.style.maxHeight = `${Math.max(MENU_LEAST, down ? below : above)}px`;
    // Placed by hand, because the list is a fixed layer: the column it hangs
    // in scrolls when the window is short, and a list positioned inside it
    // would be cut off at the fold. See `.drop-menu`.
    menu.style.left = `${box.left}px`;
    menu.style.width = `${box.width}px`;
    menu.style.top = down ? `${box.bottom + 3}px` : "auto";
    menu.style.bottom = down ? "auto" : `${window.innerHeight - box.top + 3}px`;
    paint();
  };

  const close = () => {
    menu.hidden = true;
    face.setAttribute("aria-expanded", "false");
    cursor = -1;
  };

  const isOpen = () => !menu.hidden;

  const paint = () => {
    items.forEach((li, i) => li.classList.toggle("at", i === cursor));
    if (items[cursor]) items[cursor].scrollIntoView({ block: "nearest" });
  };

  const commit = (i) => {
    close();
    if (i < 0 || i >= joins.length || i === at) return;
    stopPlay(false);
    at = i;
    showJoin();
  };

  face.addEventListener("mousedown", (ev) => {
    // A press on the list is the list's own. It arrives here because the
    // list hangs off the face -- and taken as a press on the face it shut
    // the list before the click that was choosing a join could land on it,
    // which is a picker the mouse cannot pick with.
    if (ev.target.closest(".drop-menu")) return;
    // The focus is taken by hand: the window's own keys are on `window`, and
    // a picker that has just been clicked has to be the one answering them.
    ev.preventDefault();
    face.focus();
    isOpen() ? close() : open();
  });

  menu.addEventListener("mousemove", (ev) => {
    const li = ev.target.closest("li");
    if (li && items.indexOf(li) !== cursor) {
      cursor = items.indexOf(li);
      paint();
    }
  });

  menu.addEventListener("click", (ev) => {
    const li = ev.target.closest("li");
    if (li) commit(items.indexOf(li));
  });

  // Anywhere else, and it is not a choice being made -- and so is anything
  // that moves what the list is standing over.
  window.addEventListener("mousedown", (ev) => {
    if (!ev.target.closest(".pairpick")) close();
  });
  window.addEventListener("wheel", close, true);
  window.addEventListener("scroll", close, true);

  face.addEventListener("keydown", (ev) => {
    const step = (dir) => {
      cursor = clamp(cursor + dir, 0, items.length - 1);
      paint();
    };
    if (!isOpen()) {
      // Closed, the arrows move between joins without the list coming down
      // -- eleven joins set one after another is what this panel is used
      // for. Everything else the window's own keys still answer to.
      if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
        const next = clamp(at + (ev.key === "ArrowDown" ? 1 : -1), 0, joins.length - 1);
        commit(next);
      } else if (ev.key === "Enter" || ev.key === " ") {
        open();
      } else {
        return;
      }
    } else {
      switch (ev.key) {
        case "ArrowDown":
          step(1);
          break;
        case "ArrowUp":
          step(-1);
          break;
        case "Home":
        case "End":
          cursor = ev.key === "Home" ? 0 : items.length - 1;
          paint();
          break;
        case "Enter":
        case " ":
          commit(cursor);
          break;
        case "Escape":
          close();
          break;
        default:
          // Tab included: it is leaving, and leaving should still work.
          close();
          return;
      }
    }
    // Swallowed, so that the window does not answer the same key a second
    // time: Escape while the list is down closes the list, not the window,
    // and Enter picks a join rather than pressing OK.
    ev.preventDefault();
    ev.stopPropagation();
  });
}

pairPicker();
// And the three `<select>`s beside it -- 効果 and the two easing pickers.
// The same lists the rest of the program draws for itself: see `wireDrops`.
wireDrops();

/// Put what is in hand on every join in the list. Twelve episodes want the
/// same crossing twelve times, and setting it twelve times is eleven times
/// too many.
el("x-all").addEventListener("click", () => {
  const c = crossing();
  if (!c) return;
  for (const j of joins) j.after = { ...c };
  paintNote();
});

el("x-clear").addEventListener("click", () => {
  for (const j of joins) j.after = null;
  drawPattern();
  paintLive();
  showJoin();
});

el("play").addEventListener("click", () => (playing ? stopPlay() : startPlay()));

const step = (n) => {
  stopPlay(false);
  const gap = 1 / fps();
  head = clamp(head + n * gap, 0, span ? span.seconds : 0);
  drawScrub();
  showFrame(head);
};
el("step-back").addEventListener("click", () => step(-1));
el("step-fwd").addEventListener("click", () => step(1));
el("go-start").addEventListener("click", () => {
  stopPlay(false);
  head = 0;
  drawScrub();
  showFrame(head);
});
el("go-end").addEventListener("click", () => {
  stopPlay(false);
  head = span ? span.seconds : 0;
  drawScrub();
  showFrame(head);
});

el("loop").addEventListener("click", () => {
  looping = !looping;
  el("loop").classList.toggle("on", looping);
  el("loop").setAttribute("aria-pressed", looping ? "true" : "false");
});

// The scrubber answers a press and a drag alike: a crossing is a second or
// two, and finding the instant to look at inside it is done by dragging.
let dragging = false;
const scrubAt = (ev) => {
  const box = el("xscrub").getBoundingClientRect();
  const total = span && span.seconds > 0 ? span.seconds : 1;
  return clamp(((ev.clientX - box.left) / Math.max(1, box.width)) * total, 0, total);
};
el("xscrub").addEventListener("pointerdown", (ev) => {
  stopPlay(false);
  dragging = true;
  el("xscrub").setPointerCapture(ev.pointerId);
  head = scrubAt(ev);
  drawScrub();
  showFrame(head);
});
el("xscrub").addEventListener("pointermove", (ev) => {
  if (!dragging) return;
  head = scrubAt(ev);
  drawScrub();
  showFrame(head);
});
const endDrag = () => (dragging = false);
el("xscrub").addEventListener("pointerup", endDrag);
el("xscrub").addEventListener("pointercancel", endDrag);

// --- the sound -----------------------------------------------------------

const gain = (v) => (v / 100) ** 2;
const muted = () => el("mute").classList.contains("muted");

function showVolume(level, silent, remember = true) {
  const v = clamp(Math.round(Number(level)), 0, 100);
  el("volume").value = String(v);
  el("volume").style.setProperty("--at", `${v}%`);
  el("vol-num").textContent = `${v}%`;
  el("mute").classList.toggle("muted", silent);
  el("mute").setAttribute("aria-pressed", silent ? "true" : "false");
  if (invoke) invoke("set_volume", { level: silent ? 0 : gain(v) }).catch(() => {});
  if (!remember) return;
  prefs.set("volume", v);
  prefs.set("muted", silent);
}
el("volume").addEventListener("input", () =>
  showVolume(el("volume").value, muted() && Math.round(Number(el("volume").value)) <= 0),
);
el("mute").addEventListener("click", () => showVolume(el("volume").value, !muted()));

// --- leaving the window --------------------------------------------------

/// OK hands the settings back. Every join, not only the one in hand: 一括適用
/// writes to all of them, and so does 全解除.
async function done() {
  stopPlay(false);
  // **Awaited before the window is closed.** `emit` is a trip to the backend
  // and back out to the other window, and a window destroyed while that trip
  // is in flight takes the message with it: OK closed the window and the
  // list kept the settings it had. The close is what this window is for at
  // this point, so it waits.
  if (emit) {
    await emit("cross-done", {
      joins: joins.map((j) => ({ id: j.id, after: j.after })),
    }).catch((e) => jlog(`cross-done: ${e}`));
  }
  invoke && invoke("close_cross");
}

el("cross-ok").addEventListener("click", done);
el("cross-cancel").addEventListener("click", () => {
  stopPlay(false);
  invoke && invoke("close_cross");
});

// Enter is OK and Escape is キャンセル, as they are in every dialog. Not while
// a field has the focus and something is being typed into it: Enter in the
// seconds field means "take this number".
window.addEventListener("keydown", (ev) => {
  const tag = ev.target && ev.target.tagName;
  const typing = tag === "INPUT" && ev.target.type !== "range";
  // A button or a list that has the focus answers Enter itself: キャンセル
  // with the focus on it is not OK.
  const own = tag === "BUTTON" || tag === "SELECT" || tag === "TEXTAREA";
  if (ev.key === "Escape") {
    ev.preventDefault();
    invoke && invoke("close_cross");
  } else if (ev.key === "Enter" && !typing && !own) {
    ev.preventDefault();
    done();
  } else if (ev.key === " " && !typing) {
    ev.preventDefault();
    playing ? stopPlay() : startPlay();
  }
});

// --- what the list says --------------------------------------------------

if (listen) {
  hear("cross-open", (ev) => {
    answered = true;
    const said = ev.payload || {};
    const theirs = Array.isArray(said.joins) ? said.joins : [];
    // Sent again to a window already open -- the list's button pressed a
    // second time. Where it is the same joins, what has been changed in here
    // and not yet OK'd is kept, and only the join picked moves.
    const same =
      theirs.length === joins.length &&
      theirs.every(
        (j, k) => j.beforePath === joins[k].beforePath && j.afterPath === joins[k].afterPath,
      );
    if (!(changedHere && same)) {
      joins = theirs;
      changedHere = false;
    }
    at = clamp(Number(said.pick) || 0, 0, Math.max(0, joins.length - 1));
    // Nothing to build for the picker: its list is drawn on the way down, so
    // it is always the joins as they are now and in the language the window
    // is in now.
    showJoin();
  });

  hear("cross-play-ended", (ev) => {
    if (ev.payload !== playRun) return;
    setPlaying(false);
    if (looping) {
      head = 0;
      startPlay();
    } else {
      // Left standing on the last instant, which is what somebody watching a
      // crossing end is looking at.
      showFrame(head);
    }
  });

  hear("audio-error", (ev) => {
    el("note").textContent = tr("editor.audioFailed", { e: ev.payload });
  });

  hear("lang-changed", (ev) => {
    setLang(ev.payload, false);
  });
}

// Only the words change. Loading the join again for them stopped a crossing
// being played and put the head back at its start.
onLangChange(() => {
  updateReadouts();
  paintNote();
  const j = join();
  if (j) nameWindow(j);
});

window.addEventListener("resize", () => {
  drawScrub();
  if (!playing) showFrame(head);
});

noBrowserMenu();
noNativeDrag();
applyStatic();
showVolume(prefs.get("volume"), !!prefs.get("muted"), false);
setPlaying(false);
drawScrub();
drawPattern();

/// Ask the list for a join, and go on asking until one arrives.
///
/// The same handshake 拡大表示 makes, for the same reason: a listener is only
/// in the backend's registry once the call that asked for it has been there
/// and back, so a window that announces itself from its last line can be
/// answered before it can hear the answer.
async function announceReady() {
  if (!emit) return;
  await Promise.all(wiring).catch((e) => jlog(`cross wiring: ${e}`));
  for (let i = 0; i < 8 && !answered; i++) {
    emit("cross-ready", null);
    await new Promise((go) => setTimeout(go, 400));
  }
}

announceReady();
