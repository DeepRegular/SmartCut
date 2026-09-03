// The list window: the clips, what is to become of them, and the batch that
// writes them out.
//
// Arranged after TMPGEnc MPEG Smart Renderer 6, which splits the same job
// into 入力設定 → カット編集 → 出力設定 → 出力. Three of those four are screens
// of this window; カット編集 is a window of its own (`editor.html` /
// `main.js`), because cutting is done to one clip rather than settled once
// for the list, and it wants a moment that says "done with this one". That
// moment is its OK button.
//
// This window is the one that keeps working while you are not looking at it.
// Adding a clip queues a pass over it -- the same seek index the editor would
// otherwise build the moment you opened it -- and Ctrl+D queues a commercial
// detection on top. Both run against a recording opened straight off disc,
// sharing nothing with the editor, so a long pass over clip 12 costs the clip
// you are cutting nothing.
//
// Which is why none of it stops for the editor. An index, a commercial
// detection and a cut editor open on a third recording all run at once: two
// lanes here and a window of its own, and the passes hold themselves to part
// of the machine while that window is up.

import { fmt, clock, coarse, chLabel, cmNote, esc, noBrowserMenu } from "./shared.js";
import { t, applyStatic, preference, currentLang, setLang, onLangChange, tellBackend, confirmWithOs }
  from "./i18n.js";

const T = window.__TAURI__ || {};
const invoke = T.core && T.core.invoke;
const listen = T.event && T.event.listen;
const emit = T.event && T.event.emit;
const dialog = T.dialog;
const jlog = (m) => invoke && invoke("log", { msg: String(m) });
const el = (id) => document.getElementById(id);

// A webview on a headless box has no console anyone can open, and this
// window's handlers are all event callbacks -- an exception in one leaves no
// trace at all otherwise, just a click that did nothing.
window.addEventListener("error", (e) => jlog(`error ${e.message} @${e.filename}:${e.lineno}`));
window.addEventListener("unhandledrejection", (e) => jlog(`reject ${e.reason}`));

const VIDEO_EXT = ["ts", "m2ts", "mts", "m2t", "mp4", "mkv", "mov", "m4v"];
const extOf = (p) => (p.match(/\.([A-Za-z0-9]+)$/)?.[1] || "").toLowerCase();
const nameOf = (p) => p.split(/[/\\]/).pop();
const dirOf = (p) => p.slice(0, Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\")) + 1);
const stemOf = (p) => nameOf(p).replace(/\.[^.]*$/, "");
const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));

/// Which copy of its recording a row is, counting from one, or "" when that
/// recording is in the list only once.
///
/// The one thing that tells two duplicates apart, so it goes in both places
/// it is needed: the name in the list, and the filename written out. Counted
/// off the list every time rather than stamped on at duplication, so removing
/// one copy gives the survivor its plain name back.
function copyNo(clip) {
  const same = clips.filter((c) => c.path === clip.path);
  return same.length > 1 ? String(same.indexOf(clip) + 1) : "";
}

/// What to call a row anywhere it is named to the user. Two duplicates share
/// a filename, so the name alone stops identifying which one is meant the
/// moment there are two of them.
function clipLabel(clip) {
  const n = copyNo(clip);
  return n ? t("list.copyLabel", { name: clip.name, n }) : clip.name;
}

// --- the clip list ------------------------------------------------------

/// One row. `info` arrives when the index pass finishes; everything before
/// that is a filename and a progress bar, because a recording's length,
/// shape and frame rate are not knowable without the pass that reads it.
///
/// `edit` is what the editor handed back the last time this clip left it --
/// the cuts and the marks. Kept here rather than in the editor so that going
/// back to the list is not throwing the work away.
let nextId = 1;

function makeClip(path) {
  return {
    // A row's own identity, which its path is not: the same recording can be
    // in the list more than once, cut two different ways. Everything about a
    // *row* is addressed by this; the path addresses the file, and the two
    // stopped being the same thing when clips became duplicable.
    id: nextId++,
    path,
    name: nameOf(path),
    info: null,
    state: "queued", // queued | indexing | ready | error
    phase: t("phase.queued"),
    progress: 0,
    error: "",
    cm: null, // the last CmResult for this clip
    cmState: "none", // none | queued | running | done | error
    cmPhase: "",
    /// Who wrote `cmPhase`: this window off `cm` ("run"), this window off
    /// what an earlier session left on disc ("cache"), or the editor
    /// (`null`), whose sentence arrived already written. Only the first two
    /// can be written again in another language -- see `relocalise`.
    cmSource: null,
    cmProgress: 0,
    /// Blocks found by the list that the timeline has not been shown yet.
    /// Applied on the next visit to the editor, which is the only place that
    /// knows where the material begins and can turn them into marks.
    cmPending: false,
    edit: null,
    selected: false,
    out: { state: "idle", progress: 0, note: "" }, // idle|waiting|running|done|error|skipped
    /// The plan's re-encoded segments and a frame out of each, worked out
    /// once per set of cuts for the output screen to show.
    reencode: null,
    row: null,
  };
}

let clips = [];
/// Where a range selection counts from -- the last row clicked without shift.
let anchor = -1;
const byId = (id) => clips.find((c) => c.id === id);
const byPath = (p) => clips.find((c) => c.path === p);
const selected = () => clips.filter((c) => c.selected);
const ready = () => clips.filter((c) => c.state === "ready");

// --- screens ------------------------------------------------------------

const SCREENS = { input: "screen-input", outset: "screen-outset", out: "screen-out" };
let screen = "input";

function show(name) {
  if (!SCREENS[name]) return;
  screen = name;
  for (const [key, id] of Object.entries(SCREENS)) el(id).hidden = key !== name;
  for (const b of document.querySelectorAll(".screens .tab")) {
    b.classList.toggle("active", b.dataset.screen === name);
  }
  // The menu stands on all three screens: what hangs off it -- the project,
  // the preferences, the about box -- is about the program rather than about
  // whichever screen is up, and a project is as much the output settings as
  // it is the cuts. Shut on the way across, or a screen change under an open
  // menu would leave it to reappear later.
  showMenu(false);
  if (name === "outset") renderOutset();
  if (name === "out") renderOutScreen();
}

for (const b of document.querySelectorAll(".screens .tab")) {
  b.addEventListener("click", () => !b.disabled && show(b.dataset.screen));
}

// --- the cut editor, in its own window -----------------------------------
//
// The window is built in Rust (`open_editor`) and filled over the wire. The
// handshake is: it says `editor-ready` when its page is up, this window
// answers with `editor-open` naming the clip, and from then on the editor
// reports every change back as `editor-state` -- so what the list holds is
// never behind what is on screen in there, and closing that window by its
// title bar loses nothing.

/// The clip the editor is on, or is about to be.
let editing = null;
/// What that clip's edit looked like before the editor was opened on it, so
/// that キャンセル has something to put back.
let before = null;

async function edit(clip) {
  jlog(`edit ${clip.name} (${clip.state})`);
  // A clip that could not be read has nothing to open. Anything else can be
  // opened whenever it is asked for -- the editor makes its own way through a
  // recording that has never been read, showing what it has got to as it
  // goes, so waiting for the list's turn buys nothing.
  if (clip.state === "error") {
    note(t("list.cannotRead", { clip: clipLabel(clip) }));
    return;
  }
  editing = clip;
  before = clip.edit ? JSON.parse(JSON.stringify(clip.edit)) : null;
  // The index lane may be on this very clip. Two passes over one file at
  // once is the one thing worth avoiding, and it is the lane's that goes:
  // the editor is about to make the same pass and hands its pictures to the
  // film strip as it makes them. The row falls back to 解析待ち, is left
  // alone while the editor has it, and when the editor gives it back the
  // index the editor wrote is on disc, so the lane's pass is a read.
  if (clip.state === "indexing") await invoke("stop_batch", { lane: "index" });
  try {
    await invoke("open_editor", { title: t("editor.windowTitle", { clip: clipLabel(clip) }) });
    // Lost if the window is still starting up, which is what `editor-ready`
    // is for; sent anyway for the case where it is already open on another
    // clip and there will be no `editor-ready` at all.
    tellEditor();
  } catch (e) {
    note(t("list.cannotOpenEditor", { e }));
    editing = null;
    before = null;
  }
  paintList();
  pump();
}

/// Say which clip, and hand over what is known about it.
///
/// Called twice for a window that had to be built: once as soon as it exists,
/// which the page is usually still starting up to hear, and again when it
/// says `editor-ready`. Both carry the same thing, and neither may *spend*
/// anything -- an emit that nobody was listening for must leave the state it
/// described exactly as it found it, or the second call describes less than
/// the first. `cmPending` is cleared where it is actually met: on the
/// `editor-state` that comes back with the detection's note on it.
function tellEditor() {
  if (!editing || !emit) return;
  emit("editor-open", {
    id: editing.id,
    path: editing.path,
    saved: editing.edit,
    // Blocks a batch detection found that the timeline has not been shown
    // yet. Only the editor can turn them into marks -- it is the one that
    // knows where the material begins.
    cm:
      editing.cmPending && editing.cm
        ? { blocks: editing.cm.blocks, note: editing.cmPhase }
        : null,
  });
}

if (listen) {
  listen("editor-ready", () => tellEditor());

  // The editor as it works, not only when it is done.
  listen("editor-state", (ev) => {
    const state = ev.payload;
    const clip = byId(state.id);
    if (!clip) return;
    // A detection landing on the timeline: either the marks this window
    // handed over at `editor-open` (`cmPending`), or one run from inside the
    // editor, which comes back with a note the clip did not have before.
    const landed = state.cmNote && (clip.cmPending || state.cmNote !== clip.cmPhase);
    clip.edit = state;
    if (state.cmNote) {
      clip.cmPhase = state.cmNote;
      clip.cmState = "done";
      clip.cmPending = false;
      clip.cmSource = null;
    }
    // **A detection is not an edit, so キャンセル does not undo it.** It is
    // minutes of reading the recording, asked for by its own button and out
    // here as often as in there, and the marks it leaves are its answer
    // rather than something anybody did to the clip. So the state it landed
    // in becomes what cancelling goes back to. Without this, detecting from
    // the list and then opening the editor to look and backing out lost the
    // marks -- and lost them for good, since the finding had already been
    // handed over and would not be offered again.
    if (landed && clip === editing) before = JSON.parse(JSON.stringify(state));
    paintRow(clip);
    // The other way a project changes: a cut made in the other window. It
    // repaints one row rather than the list, so it says so itself.
    touch();
    if (screen === "outset") renderOutset();
    if (screen === "out") renderOutScreen();
  });

  listen("editor-cancel", (ev) => {
    const clip = byId(ev.payload) || editing;
    if (clip) clip.edit = before;
    paintList();
  });

  // The editor window has gone -- by OK, by キャンセル, or by its own cross.
  // Whichever it was, what it did is already here. What is left is the row
  // itself: it was passed over while the editor had it, and if it was never
  // read the lane can have it now -- cheaply, since the editor will have
  // written its seek index.
  listen("editor-closed", () => {
    editing = null;
    before = null;
    paintList();
    pump();
  });
}

// --- adding clips -------------------------------------------------------

async function addPaths(inputs) {
  // Everything the list is handed goes through the backend first: a share
  // named the way it is written down -- smb://nas/rec/a.ts, \\nas\rec\a.ts --
  // has to become the mount point it is under before anything can open it,
  // and a folder stands for the files in it, which is how a night's
  // recordings arrive as one line pasted from the NAS.
  let resolved;
  try {
    resolved = await invoke("resolve_paths", { paths: inputs });
  } catch (e) {
    note(`${e}`);
    return [];
  }
  const failed = resolved.filter((r) => r.error);
  const paths = resolved.flatMap((r) => r.files);
  const taken = [];
  const skipped = [];
  for (const p of paths) {
    if (!VIDEO_EXT.includes(extOf(p))) {
      skipped.push(nameOf(p));
      continue;
    }
    if (byPath(p)) continue;
    const clip = makeClip(p);
    clips.push(clip);
    taken.push(clip);
  }
  renderList();
  // One at a time rather than a hundred at once: each is a stat on whatever
  // the recordings are on, and the answer is wanted before anyone looks at
  // the row rather than this instant.
  (async () => {
    for (const clip of taken) await restoreCm(clip);
  })();
  // A path that could not be reached at all is the more useful thing to say,
  // so it wins the one line there is.
  if (failed.length) {
    note(failed[0].error + (failed.length > 1 ? t("list.andMore", { n: failed.length - 1 }) : ""));
  } else if (skipped.length) {
    note(
      t("list.unsupported", { names: skipped.slice(0, 3).join(", ") }) +
        (skipped.length > 3 ? t("list.andMore", { n: skipped.length - 3 }) : "")
    );
  }
  if (taken.length) pump();
  return taken;
}

el("add-files").addEventListener("click", async () => {
  const picked = await dialog.open({
    multiple: true,
    filters: [{ name: t("dialog.video"), extensions: VIDEO_EXT }],
  });
  if (!picked) return;
  await addPaths(Array.isArray(picked) ? picked : [picked]);
});

// Dropping files. Tauri intercepts the drag before the page sees it, so the
// window's own events are what carry the paths -- the HTML5 ones never fire
// with `dragDropEnabled`, which is the default and is what lets a drop
// anywhere in the window count.
if (listen) {
  listen("tauri://drag-enter", () => el("droptarget").classList.add("over"));
  listen("tauri://drag-leave", () => el("droptarget").classList.remove("over"));
  listen("tauri://drag-drop", (ev) => {
    el("droptarget").classList.remove("over");
    const paths = ev.payload?.paths || [];
    if (!paths.length) return;
    // A project dropped on the window is a project being opened. One at a
    // time and nothing else with it: two projects are two lists, and a
    // project alongside recordings does not say whether the recordings are
    // to be added to it or dropped instead of it.
    if (paths.length === 1 && extOf(paths[0]) === PROJECT_EXT) {
      openDroppedProject(paths[0]);
      return;
    }
    // A drop is a way of adding clips, so it lands on the list whichever
    // screen was up when it happened.
    show("input");
    addPaths(paths);
  });
}

// --- the queue ----------------------------------------------------------
//
// Two lanes, each working down the list in order: one builds seek indexes,
// the other detects commercials. One pass of each kind at a time, and the
// two of them run together.
//
// Two rather than one because they are not the same load. Building an index
// decodes every key picture, which is the machine's cores; a detection reads
// the caption stream, or the audio and a logo, and libavcodec threads none of
// those -- so the detection is one core and a great deal of waiting for the
// disc. Running them side by side costs the index pass much less than the
// detection gains, which is what makes an evening's Ctrl+D finish overnight.
//
// Two rather than more because a third pass would be a second decoder on the
// same cores, and beyond that the disc is the wall anyway: three answers
// later is not better than two answers sooner and the third one after them.
//
// The lanes run whether or not the cut editor is open. What gives way to the
// editor is not the work but its share of the machine -- `background_threads`
// on the Rust side hands the passes part of it while that window is up.

/// Which lanes have a pass in flight, so neither is started twice.
const lanes = { index: false, cm: false };
const running = () => lanes.index || lanes.cm;

/// Raised by 解析を中止 and while an export is running. Not the same as an
/// empty queue: the work is still queued, it is just not being taken.
let paused = false;

/// The last thing worth saying that was not a lane saying it -- an error, or
/// what 中止 did. Shown when neither lane has anything to report.
let sticky = "";

function note(text) {
  sticky = text;
  paintQueueNote();
}

/// One line, two lanes. Composed from what is running rather than written by
/// whichever pass spoke last, because both want this line and neither may
/// have it to itself.
function paintQueueNote() {
  const bits = [];
  const ix = clips.find((c) => c.state === "indexing");
  const cm = clips.find((c) => c.cmState === "running");
  if (ix) bits.push(t("queue.indexing", { clip: clipLabel(ix) }));
  if (cm) bits.push(t("queue.detecting", { clip: clipLabel(cm) }));
  el("queue-note").textContent = bits.length ? bits.join(t("sep")) : sticky;
}

/// The next clip for a lane, or nothing.
///
/// The clip in the editor is passed over by the index lane: the editor is
/// making that pass itself. Nothing is passed over by the detection lane --
/// it reads a different part of the file, and detecting the commercials of
/// the clip you are cutting while you cut it is the point of Ctrl+D.
function nextFor(lane) {
  if (lane === "index") {
    return clips.find((c) => c.state === "queued" && c !== editing);
  }
  return clips.find((c) => c.cmState === "queued");
}

function pump() {
  pumpLane("index");
  pumpLane("cm");
}

async function pumpLane(lane) {
  if (lanes[lane] || paused) return;
  lanes[lane] = true;
  try {
    for (;;) {
      if (paused) break;
      const next = nextFor(lane);
      if (!next) break;
      // Whatever the note last said, this lane is working now.
      sticky = "";
      if (lane === "index") await runIndex(next);
      else await runCm(next);
    }
  } finally {
    lanes[lane] = false;
    paintButtons();
    paintQueueNote();
  }
}

async function runIndex(clip) {
  clip.state = "indexing";
  clip.phase = t("phase.reading");
  clip.progress = 0;
  paintRow(clip);
  paintQueueNote();
  try {
    clip.info = await invoke("index_clip", { path: clip.path });
    clip.state = "ready";
    clip.progress = 1;
    clip.phase = clip.info.cached
      ? t("phase.indexReused")
      : t("phase.indexBuilt", { s: clip.info.seconds.toFixed(0) });
  } catch (e) {
    if (String(e).includes("cancelled")) {
      // Put back, not failed: 中止 means "not now", and the pass left
      // nothing behind to be inconsistent about.
      clip.state = "queued";
      clip.phase = t("phase.stopped");
    } else {
      clip.state = "error";
      clip.error = String(e);
    }
  }
  paintRow(clip);
  paintTotals();
  paintButtons();
  paintQueueNote();
  if (clip.selected) paintProps();
}

/// Put back what an earlier session detected in this recording, if it did.
///
/// A detection is minutes of reading the file and it is the same answer every
/// time, so the backend writes it down; this is the list picking it up again.
/// What comes back is the whole finding and not merely a mark that one was
/// made -- the blocks go to the editor on the next visit exactly as a fresh
/// detection's would, because they *are* that detection.
///
/// Nothing is said when there is nothing to say: the usual answer for a
/// recording added for the first time is `null`, and a row that has never
/// been detected should look like one.
///
/// `pending` is for rows that came out of a project file, which knows
/// something the cache cannot: whether those blocks have already been shown
/// to the timeline. The cache can only say that a detection was once run.
async function restoreCm(clip, pending = null) {
  if (!invoke) return;
  let res;
  try {
    res = await invoke("cm_cached", { path: clip.path });
  } catch {
    // A recording that cannot be reached is the index pass's news to break,
    // and it is about to. Nothing here is worth a second line about it.
    return;
  }
  // Detected while the answer was being fetched, which is a slow share and a
  // quick Ctrl+D. The pass that has just run is the newer answer.
  if (!res || clip.cmState !== "none") return;
  clip.cm = res;
  clip.cmState = "done";
  // Said as a detection that has already happened rather than as one that
  // just did: the difference matters to someone looking at a list they left
  // open overnight and wondering what it has been doing.
  clip.cmPhase = t("cm.previous", { note: cmNote(res) });
  clip.cmSource = "cache";
  clip.cmPending = pending === null ? res.blocks.length > 0 : pending;
  paintRow(clip);
  paintButtons();
  if (clip.selected) paintProps();
}

async function runCm(clip) {
  clip.cmState = "running";
  clip.cmProgress = 0;
  clip.cmPhase = t("phase.detecting");
  paintRow(clip);
  paintQueueNote();
  try {
    const res = await invoke("detect_cm_at", { path: clip.path });
    clip.cm = res;
    clip.cmState = "done";
    clip.cmPhase = cmNote(res);
    clip.cmSource = "run";
    // The marks themselves need to know where the material starts, which is
    // the editor's business; the list only carries the finding across.
    clip.cmPending = res.blocks.length > 0;
    // And carries it across *now* if that window is already open on this
    // clip, which it can be: the lanes no longer stand aside for the editor,
    // so a detection can finish while its clip is being cut. `editor-open` is
    // the only way in, and the editor will not ask a second time.
    if (clip === editing) tellEditor();
  } catch (e) {
    if (String(e).includes("cancelled")) {
      clip.cmState = "queued";
      clip.cmPhase = t("phase.stopped");
    } else {
      clip.cmState = "error";
      clip.cmPhase = t("cm.failed", { e });
    }
  }
  paintRow(clip);
  paintButtons();
  paintQueueNote();
  if (clip.selected) paintProps();
}

if (listen) {
  listen("clip-progress", (ev) => {
    const [path, phase, done] = ev.payload;
    // The row that is being read, not merely the first one holding that
    // path: the same recording can be in the list twice.
    const clip = clips.find((c) => c.path === path && c.state === "indexing");
    if (!clip) return;
    clip.phase = phase;
    clip.progress = done;
    paintRow(clip);
  });
  listen("clip-cm-progress", (ev) => {
    const [path, phase, done] = ev.payload;
    const clip = clips.find((c) => c.path === path && c.cmState === "running");
    if (!clip) return;
    clip.cmPhase = phase;
    clip.cmProgress = done;
    paintRow(clip);
  });
}

// --- drawing the list ---------------------------------------------------

/// Rebuild every row. Called when the set of clips changes -- and only then,
/// because a progress event arrives twice a second per clip and rebuilding
/// the list under the pointer would make it impossible to click anything.
function renderList() {
  // The rows a drag is holding are about to be thrown away, so it ends here
  // whether or not it had landed.
  clearDrag();
  const list = el("cliplist");
  list.innerHTML = "";
  for (const clip of clips) {
    const li = document.createElement("li");
    li.className = "clip";
    li.innerHTML = `
      <span class="n"></span>
      <img class="poster" alt="" />
      <div class="meta">
        <div class="nm"></div>
        <div class="sub dim"></div>
        <div class="cm dim"></div>
      </div>
      <div class="stat">
        <div class="badges">
          <span class="cmbadge" hidden></span>
          <span class="badge"></span>
        </div>
        <div class="pbar"><span></span></div>
        <div class="ptext dim"></div>
      </div>
      <button class="kill" title="${esc(t("list.kill"))}">×</button>`;
    li.querySelector(".poster").draggable = false;
    li.addEventListener("mousedown", (ev) => {
      if (ev.target.classList.contains("kill")) return;
      pressRow(clip, ev);
    });
    li.addEventListener("dblclick", () => edit(clip));
    li.querySelector(".kill").addEventListener("click", (ev) => {
      ev.stopPropagation();
      remove([clip]);
    });
    clip.row = li;
    list.appendChild(li);
  }
  el("drop-hint").hidden = clips.length > 0;
  paintList();
}

function paintList() {
  clips.forEach((c) => paintRow(c));
  paintTotals();
  paintButtons();
  paintProps();
  // Everything that adds, removes, reorders or duplicates a row ends here,
  // so this is where the title finds out whether there is work to save.
  touch();
}

/// Write `text` into `node` only when it is not already what is there.
///
/// Assigning `textContent` replaces the text node whether or not the text
/// changed, and a row is repainted on every progress event -- twice a second
/// per clip while the list is being read. Beyond the wasted work, a text node
/// swapped out from under the pointer between the two halves of a double
/// click leaves the two clicks with different targets, and WebKit then fires
/// no `dblclick` at all: the row stops opening the editor for as long as
/// anything is being read.
function setText(node, text) {
  if (node.textContent !== text) node.textContent = text;
}

function paintRow(clip) {
  const li = clip.row;
  if (!li) return;
  li.classList.toggle("on", clip.selected);
  li.classList.toggle("bad", clip.state === "error");
  setText(li.querySelector(".n"), String(clips.indexOf(clip) + 1));
  const img = li.querySelector(".poster");
  const poster = clip.info && clip.info.poster;
  if (poster && img.src !== poster) img.src = poster;
  img.classList.toggle("blank", !poster);
  setText(li.querySelector(".nm"), clipLabel(clip));

  const i = clip.info;
  setText(
    li.querySelector(".sub"),
    i
      ? t("row.sub", {
          len: coarse(i.duration),
          frames: i.frames,
          end: fmt(i.duration),
          w: i.width,
          h: i.height,
          fps: i.fps.toFixed(2),
          codec: i.codec,
          audio: i.has_audio ? "" : t("row.noAudio"),
        })
      : clip.state === "error"
        ? clip.error
        : clip.path
  );

  // Two things worth saying about a clip below its name: what the detection
  // found, and how much of it the edit takes out. Both are about the clip and
  // neither is about the file, which is what the line above is for.
  const bits = [];
  if (clip.cmState === "running") {
    bits.push(
      t("row.cmRunning", { pct: Math.round(clip.cmProgress * 100), phase: clip.cmPhase })
    );
  } else if (clip.cmState === "queued") bits.push(t("row.cmQueued"));
  else if (clip.cmPhase) bits.push(t("row.cmNote", { note: clip.cmPhase }));
  const cutCount = clip.edit ? clip.edit.cuts.length : 0;
  if (cutCount && i) {
    const kept = keepsOf(clip).reduce((n, k) => n + (k.b - k.a), 0);
    bits.push(t("row.cuts", { n: cutCount, kept: fmt(kept) }));
  }
  if (clip.edit && clip.edit.keyframes.length) {
    bits.push(t("row.keyframes", { n: clip.edit.keyframes.length }));
  }
  setText(li.querySelector(".cm"), bits.join(t("sep")));

  // Being edited is worth saying over anything else the row could say: it
  // is the one state that is about where the clip is rather than what has
  // been worked out about it, and it is why the index lane has walked past.
  const badge = li.querySelector(".badge");
  const state = clip === editing ? "editing" : clip.state;
  setText(
    badge,
    {
      ready: t("badge.smart"),
      error: t("badge.error"),
      indexing: t("badge.indexing"),
      editing: t("badge.editing"),
    }[state] || t("badge.queued")
  );
  badge.className = `badge ${state}`;

  // And whether the commercials have been looked for, which the line under
  // the name says already but only to someone reading it. A list of twenty
  // recordings is scanned, not read, and the one thing being looked for in
  // that scan is which of them still owe a detection -- so it goes where the
  // eye is already going, beside the state badge.
  //
  // The count is the editor's where the clip has been through it, because
  // that is what the timeline actually holds; otherwise it is the finding's
  // own. Nothing is shown while a detection is queued or running: the bar
  // below is saying that, and saying it twice would make the badge mean
  // "asked for" rather than "done".
  const cmBadge = li.querySelector(".cmbadge");
  const blocks =
    clip.edit && clip.edit.cmBlocks
      ? clip.edit.cmBlocks.length
      : clip.cm
        ? clip.cm.blocks.length
        : null;
  const detected = clip.cmState === "done" && blocks !== null;
  cmBadge.hidden = !detected;
  if (detected) {
    setText(cmBadge, blocks ? t("badge.cm", { n: blocks }) : t("badge.cmNone"));
    cmBadge.className = `cmbadge ${blocks ? "found" : "empty"}`;
  }

  const running = clip.state === "indexing" || clip.cmState === "running";
  const pct = clip.state === "indexing" ? clip.progress : clip.cmProgress;
  li.querySelector(".pbar").hidden = !running;
  li.querySelector(".pbar span").style.width = `${Math.round(pct * 100)}%`;
  setText(
    li.querySelector(".ptext"),
    running
      ? t("ptext.running", {
          phase: clip.phase && clip.state === "indexing" ? clip.phase : t("ptext.cm"),
          pct: Math.round(pct * 100),
        })
      : clip.state === "queued"
        ? t("phase.queued")
        : clip.phase
  );
}

function paintTotals() {
  const known = clips.filter((c) => c.info);
  const total = known.reduce((n, c) => n + c.info.duration, 0);
  const pending = clips.length - known.length;
  el("clip-total").textContent =
    t("input.total", { n: clips.length, t: coarse(total) }) +
    (pending ? t("input.totalPending", { n: pending }) : "");
}

function paintButtons() {
  const n = selected().length;
  const busy = running();
  // Anything but a clip that could not be read: the editor makes its own way
  // through one the list has not got to yet.
  el("edit-clip").disabled = n !== 1 || selected()[0].state === "error";
  el("duplicate-clip").disabled = n === 0;
  el("detect-selected").disabled = !selected().some((c) => c.state === "ready");
  const queued = clips.some((c) => c.state === "queued" || c.cmState === "queued");
  el("stop-batch").disabled = !busy && !(paused && queued);
  el("stop-batch").textContent = t(paused && queued && !busy ? "side.resumeBatch" : "side.stopBatch");
  el("move-up").disabled = n === 0;
  el("move-down").disabled = n === 0;
  el("select-all").disabled = clips.length === 0;
  el("remove-clip").disabled = n === 0;
  el("remove-all").disabled = clips.length === 0;
  el("run-export").disabled = ready().length === 0 || exporting;
}

function paintProps() {
  const box = el("props");
  const picked = selected();
  if (picked.length !== 1) {
    box.className = "props-body dim";
    box.textContent = picked.length
      ? t("props.many", { n: picked.length })
      : t("props.none");
    return;
  }
  const c = picked[0];
  box.className = "props-body";
  if (!c.info) {
    box.textContent =
      c.state === "error"
        ? t("props.error", { name: c.name, error: c.error })
        : t("props.queued", { name: c.name });
    return;
  }
  const i = c.info;
  const flags = [
    t(i.interlaced ? "media.interlaced" : "media.progressive"),
    i.pulldown ? t("media.pulldown") : null,
  ]
    .filter(Boolean)
    .join(", ");
  const n = copyNo(c);
  box.textContent = t("props.body", {
    name: c.name,
    copy: n ? t("props.copyOf", { n }) : "",
    path: c.path,
    codec: i.codec,
    w: i.width,
    h: i.height,
    fps: i.fps.toFixed(2),
    flags,
    // With the channel count, because that is what says whether there is
    // anything to downmix -- and a 5.1 clip in a list of stereo ones is
    // otherwise indistinguishable until the output has already been written.
    audio: i.has_audio
      ? `${t("media.audioYes")}${i.audio_channels ? ` (${chLabel(i.audio_channels)})` : ""}`
      : t("media.audioNo"),
    len: coarse(i.duration),
    frames: i.frames,
    points: i.points,
    unusable: i.unusable_points ? t("props.unusable", { n: i.unusable_points }) : "",
    scenes: i.scenes,
    index: i.index_name,
    cm: c.cmPhase ? t("props.cm", { note: c.cmPhase }) : "",
  });
}

// --- selection ----------------------------------------------------------

function pick(clip, ev) {
  const at = clips.indexOf(clip);
  if (ev.shiftKey && anchor >= 0) {
    const [lo, hi] = [Math.min(anchor, at), Math.max(anchor, at)];
    clips.forEach((c, k) => (c.selected = k >= lo && k <= hi));
  } else if (ev.ctrlKey || ev.metaKey) {
    clip.selected = !clip.selected;
    anchor = at;
  } else {
    clips.forEach((c) => (c.selected = c === clip));
    anchor = at;
  }
  paintList();
}

/// Put a second row on the same recording, carrying everything already known
/// about it.
///
/// What a duplicate is *for* is two cuts of one recording -- a two-hour
/// capture holding two programmes, the same file written out twice at
/// different bounds. So it starts as an exact copy, cuts and marks included:
/// the second cut is nearly always the first one moved rather than one begun
/// from nothing, and a copy that dropped the edit would make the feature
/// useless for the thing it is for.
///
/// What it does *not* copy is the row's identity or its place in an output
/// run. Everything the recording itself answers for -- its index, its length,
/// what a commercial detection found in it -- is the same file's answer and
/// comes along, so a duplicate costs no pass over the disc.
function duplicate(sources) {
  const made = [];
  for (const src of sources) {
    const copy = {
      ...src,
      id: nextId++,
      edit: src.edit ? JSON.parse(JSON.stringify(src.edit)) : null,
      cm: src.cm ? JSON.parse(JSON.stringify(src.cm)) : null,
      out: { state: "idle", progress: 0, note: "" },
      // Worked out against this row's own cuts when the output screen asks.
      reencode: null,
      row: null,
      selected: false,
    };
    // The saved edit names the row it was taken from; this is a different row.
    if (copy.edit) copy.edit.id = copy.id;
    // Beside the one it came from, not at the end: a duplicate is read as
    // "this one again", and a list that puts it three screens away is a list
    // you have to go looking in.
    clips.splice(clips.indexOf(src) + 1, 0, copy);
    made.push(copy);
  }
  if (!made.length) return;
  // Selected, and the sources not: the copy is what you are about to work on.
  clips.forEach((c) => (c.selected = false));
  made.forEach((c) => (c.selected = true));
  anchor = clips.indexOf(made[0]);
  renderList();
  made[0].row.scrollIntoView({ block: "nearest" });
  // Only a clip that was never read has anything left to do.
  pump();
}

function selectAll() {
  clips.forEach((c) => (c.selected = true));
  anchor = 0;
  paintList();
}

async function remove(doomed) {
  const gone = new Set(doomed.map((c) => c.id));
  // A clip being read right now has a pass behind it that has to be told to
  // stop, or it would go on reading a file nothing is listed against. Only
  // the lane that is on it: the other one is reading a clip that is staying.
  if (doomed.some((c) => c.state === "indexing")) {
    await invoke("stop_batch", { lane: "index" });
  }
  if (doomed.some((c) => c.cmState === "running")) {
    await invoke("stop_batch", { lane: "cm" });
  }
  clips = clips.filter((c) => !gone.has(c.id));
  anchor = -1;
  renderList();
  // The editor is open on a recording that is no longer in the list, so the
  // window it is in has nothing left to be about.
  if (editing && gone.has(editing.id)) {
    editing = null;
    before = null;
    await invoke("close_editor");
  }
  pump();
}

el("select-all").addEventListener("click", selectAll);
el("remove-clip").addEventListener("click", () => remove(selected()));
el("remove-all").addEventListener("click", () => remove(clips.slice()));
el("edit-clip").addEventListener("click", () => selected()[0] && edit(selected()[0]));
el("duplicate-clip").addEventListener("click", () => duplicate(selected()));
el("detect-selected").addEventListener("click", () => detectSelected());
el("stop-batch").addEventListener("click", async () => {
  if (paused) {
    paused = false;
    note("");
    pump();
    return;
  }
  paused = true;
  note(t("list.stopping"));
  await invoke("stop_batch", { lane: null });
  note(t("list.stopped"));
  paintButtons();
});

function move(dir) {
  const order = dir < 0 ? clips.map((_, i) => i) : clips.map((_, i) => clips.length - 1 - i);
  for (const i of order) {
    const j = i + dir;
    if (!clips[i].selected || j < 0 || j >= clips.length || clips[j].selected) continue;
    [clips[i], clips[j]] = [clips[j], clips[i]];
  }
  renderList();
}
el("move-up").addEventListener("click", () => move(-1));
el("move-down").addEventListener("click", () => move(1));

// --- 並べ替え（ドラッグ） -----------------------------------------------
//
// Rows are carried with plain mouse events rather than with HTML5 drag and
// drop. Tauri takes the window's drags before the page sees them -- that is
// what carries the file drop above -- so a `dragstart` inside the page is not
// something to build on. It suits the two kinds of drop being different
// things anyway: files arrive from outside, rows only ever move about inside,
// and neither can be mistaken for the other.
//
// What is carried is the selection, so the press that would pick one row may
// also be the start of carrying five. That is why a press on a row that is
// already selected leaves the selection alone until the button comes up:
// collapsing to the one row on the way down would drop the other four out of
// the drag before it began.

/// The button is down on a row, and it is not (yet) a drag.
let press = null;
/// The drag proper: which rows are being carried, and where they would land.
let drag = null;
/// How far the pointer travels before a press becomes a drag. Enough that a
/// click is still a click under an unsteady hand.
const SLOP = 4;

function pressRow(clip, ev) {
  if (ev.button !== 0) {
    pick(clip, ev);
    return;
  }
  const plain = !ev.shiftKey && !ev.ctrlKey && !ev.metaKey;
  press = { clip, x: ev.clientX, y: ev.clientY, collapse: clip.selected && plain };
  if (!press.collapse) pick(clip, ev);
}

/// Where the pointer would put the rows: an index into `clips` counted the
/// way an insertion is -- 0 above the first row, `clips.length` below the
/// last. The half-way line of a row is the point it changes at, so the rows
/// part where the pointer already is rather than where it has been.
function dropAt(y) {
  for (let i = 0; i < clips.length; i++) {
    const row = clips[i].row;
    if (!row) continue;
    const r = row.getBoundingClientRect();
    if (y < r.top + r.height / 2) return i;
  }
  return clips.length;
}

/// The carried rows dimmed, and a line where they would land. Drawn on the
/// rows themselves rather than as a floating marker: the line belongs to the
/// gap between two rows, and the gap is only ever a row's edge.
function paintDrag() {
  clips.forEach((c, i) => {
    if (!c.row) return;
    c.row.classList.toggle("dragging", !!drag && drag.ids.has(c.id));
    c.row.classList.toggle("dropbefore", !!drag && drag.at === i);
    c.row.classList.toggle(
      "dropafter",
      !!drag && drag.at === clips.length && i === clips.length - 1
    );
  });
}

function startDrag() {
  const held = press.clip.selected ? selected() : [press.clip];
  drag = { ids: new Set(held.map((c) => c.id)), at: clips.indexOf(press.clip), y: press.y };
  el("droptarget").classList.add("reordering");
  paintDrag();
  requestAnimationFrame(edgeScroll);
}

/// Reaching the ends of a long list without letting go: while the pointer is
/// held near the top or bottom of the list, the list comes to it.
function edgeScroll() {
  if (!drag) return;
  const wrap = el("droptarget");
  const r = wrap.getBoundingClientRect();
  const EDGE = 28;
  const over = Math.min(drag.y - (r.top + EDGE), 0) || Math.max(drag.y - (r.bottom - EDGE), 0);
  if (over) {
    const was = wrap.scrollTop;
    wrap.scrollTop += Math.max(-EDGE, Math.min(EDGE, over)) * 0.5;
    if (wrap.scrollTop !== was) {
      drag.at = dropAt(drag.y);
      paintDrag();
    }
  }
  requestAnimationFrame(edgeScroll);
}

/// Let go of the rows without moving them. Leaves the list as it was; the
/// classes go with the repaint.
function clearDrag() {
  press = null;
  if (!drag) return;
  drag = null;
  el("droptarget").classList.remove("reordering");
  paintDrag();
}

/// Take the carried rows out of the list and put them back in at the drop,
/// keeping the order they were in. The index counted rows that are being
/// carried, so what it means once they are out is however many of the rows
/// left were above it.
function endDrag() {
  const { ids, at } = drag;
  const held = clips.filter((c) => ids.has(c.id));
  const rest = clips.filter((c) => !ids.has(c.id));
  const above = clips.slice(0, at).filter((c) => !ids.has(c.id)).length;
  clips = [...rest.slice(0, above), ...held, ...rest.slice(above)];
  clearDrag();
  renderList();
}

window.addEventListener("mousemove", (ev) => {
  if (!press && !drag) return;
  if (!drag) {
    if (Math.abs(ev.clientX - press.x) + Math.abs(ev.clientY - press.y) < SLOP) return;
    startDrag();
  }
  drag.y = ev.clientY;
  drag.at = dropAt(ev.clientY);
  paintDrag();
});

window.addEventListener("mouseup", () => {
  if (drag) endDrag();
  // A press that never travelled: the selection it was holding open now
  // settles onto the one row, which is what a plain click has always meant.
  else if (press && press.collapse) pick(press.clip, {});
  press = null;
});

// Escape puts them back, and the pointer leaving the window with the button
// up is the same thing -- neither should land rows somewhere unasked.
window.addEventListener(
  "keydown",
  (ev) => {
    if (!drag || ev.key !== "Escape") return;
    ev.stopPropagation();
    clearDrag();
  },
  true
);
window.addEventListener("blur", () => clearDrag());

// The list can also be scrolled under a held drag -- with the wheel, or by
// the edge scroll above -- and the line has to follow it.
el("droptarget").addEventListener("scroll", () => {
  if (!drag) return;
  drag.at = dropAt(drag.y);
  paintDrag();
});

/// Queue a commercial detection on every selected clip that has been read.
///
/// Queued rather than run: the passes are minutes each on a broadcast
/// recording, and the detection lane takes them one at a time. Selecting
/// eighteen clips and pressing Ctrl+D is a night's work asked for in one
/// keystroke, which is the point of it -- and the indexing of the ones still
/// unread carries on beside it.
function detectSelected() {
  const want = selected().filter((c) => c.state === "ready" && c.cmState !== "running");
  if (!want.length) return;
  want.forEach((c) => {
    c.cmState = "queued";
    c.cmPhase = "";
  });
  paused = false;
  paintList();
  pump();
}

// --- the edited timeline, without the editor ----------------------------
//
// The output screens need to know what survives a clip's cuts, and the
// editor is a different window that may not even be open. Same arithmetic as
// `rebuildTimeline` in main.js, over the cuts the list was told about.

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

/// What survives, in source time, each piece carrying where it lands in the
/// output.
///
/// Starts at the first access point rather than at zero, as the editor's
/// timeline does: nothing before it can be decoded, the planner clamps to it,
/// and the output's own clock therefore starts there.
function keepsOf(clip) {
  const dur = clip.info.duration;
  const keeps = [];
  let pos = clip.info.first_point;
  for (const c of normalise(clip.edit ? clip.edit.cuts : [])) {
    if (c.a > pos + 1e-6) keeps.push({ a: pos, b: Math.min(c.a, dur) });
    pos = Math.max(pos, c.b);
  }
  if (pos < dur - 1e-6) keeps.push({ a: pos, b: dur });
  let at = 0;
  for (const k of keeps) {
    k.at = at;
    at += k.b - k.a;
  }
  return keeps;
}

const rangesOf = (clip) => keepsOf(clip).map((k) => [k.a, k.b]);

function srcToOut(keeps, s) {
  for (const k of keeps) {
    if (s >= k.a - 1e-9 && s < k.b - 1e-9) return k.at + (s - k.a);
  }
  return null;
}

// --- output settings ----------------------------------------------------

const settings = {
  dir: "",
  prefix: "cut_",
  container: "",
  audio: "smart",
  /// Empty follows the recording; anything else is a downmix, which is the
  /// one audio setting that decides the mode instead of living under it.
  audioChannels: "",
  /// Empty lets the engine derive one from the recording -- and bring it down
  /// with the channel count when there is a fold.
  audioBitrate: "",
  keyframes: false,
};

/// Where a clip will be written, given the settings.
///
/// Named after the recording it came from, in the folder chosen or beside
/// it. A broadcast file's name carries the date, the channel and the episode
/// -- everything you would need to find it again -- so throwing it away for
/// "cut.ts" is a loss. The prefix is what says which one is the edit.
///
/// Duplicated clips are numbered `_1`, `_2` in list order, because they came
/// from one recording and would otherwise be one filename written twice --
/// the second cut landing on top of the first. Counted off the list rather
/// than fixed at the moment of duplication, so deleting one copy gives the
/// survivor its plain name back.
function outputPath(clip) {
  const ext = settings.container || extOf(clip.path) || "mp4";
  const dir = settings.dir ? settings.dir.replace(/[/\\]*$/, "/") : dirOf(clip.path);
  const n = copyNo(clip);
  return `${dir}${settings.prefix}${stemOf(clip.path)}${n ? `_${n}` : ""}.${ext}`;
}

/// Which control stands for which setting. Kept because the flow is
/// otherwise one-way -- the screen is where the settings are made, and the
/// only thing that ever makes them from the other side is a project opening.
const settingInputs = [];

function bindSetting(id, key, kind = "value") {
  const input = el(id);
  const read = () => {
    settings[key] = kind === "checked" ? input.checked : input.value;
    renderOutset();
    renderOutScreen();
    touch();
  };
  input.addEventListener(kind === "checked" ? "change" : "input", read);
  settingInputs.push([input, key, kind]);
  if (kind === "checked") input.checked = settings[key];
  else input.value = settings[key];
}

/// Put `settings` back on screen, for when something other than the screen
/// has changed them.
function showSettings() {
  for (const [input, key, kind] of settingInputs) {
    if (kind === "checked") input.checked = !!settings[key];
    else input.value = settings[key];
    // A `<select>` handed a value it has no option for lands on nothing at
    // all, so it is put back on its first option -- and then the setting is
    // read back off the control in every case. What is on screen and what
    // will be written have to be one answer, and the control is the one that
    // can only hold an answer that exists.
    if (input.tagName === "SELECT" && input.selectedIndex < 0) input.selectedIndex = 0;
    settings[key] = kind === "checked" ? input.checked : input.value;
  }
  renderOutset();
  renderOutScreen();
}
bindSetting("out-dir", "dir");
bindSetting("out-prefix", "prefix");
bindSetting("out-container", "container");
bindSetting("out-audio", "audio");
bindSetting("out-audio-channels", "audioChannels");
bindSetting("out-audio-bitrate", "audioBitrate");
bindSetting("out-keyframes", "keyframes", "checked");

// --- drop-downs that open upward -----------------------------------------
//
// The file settings are the bottom panel of the window, so a native popup
// there has nowhere to go but off the screen -- the bitrate list, sixteen
// rungs of it, ran past the edge with most of itself out of reach. Where a
// native popup opens is the platform's to decide and not ours, so the popup
// is ours instead.
//
// The `<select>` stays exactly where it was and goes on holding the answer:
// everything that reads a setting off a control, puts one back on opening a
// project, or translates the options still works, because the control is
// still there. What is replaced is only what a click on it draws -- and what
// that draws has to answer a keyboard too, because the control it stands in
// for did.

/// The menu that is up: `{ hide, onKey }`. Only ever one.
let openDrop = null;

function closeDrop() {
  if (openDrop) openDrop.hide();
  openDrop = null;
}

/// Draw `select`'s options above it instead of below.
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
      menu.appendChild(li);
      return li;
    });
    at = select.selectedIndex;
    menu.hidden = false;
    openDrop = { hide: () => (menu.hidden = true), onKey };
    paint();
  };

  const commit = (i) => {
    if (items[i]) {
      select.value = items[i].dataset.value;
      // What a click on a real option would have raised, which is what every
      // setting on this screen is bound to.
      select.dispatchEvent(new Event("input", { bubbles: true }));
    }
    closeDrop();
  };

  const onKey = (ev) => {
    switch (ev.key) {
      case "ArrowDown":
      case "ArrowUp":
        at = Math.max(0, Math.min(items.length - 1, at + (ev.key === "ArrowUp" ? -1 : 1)));
        paint();
        break;
      case "Home":
      case "End":
        at = ev.key === "Home" ? 0 : items.length - 1;
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
    if (li && items.indexOf(li) !== at) {
      at = items.indexOf(li);
      paint();
    }
  });

  menu.addEventListener("click", (ev) => {
    const li = ev.target.closest("li");
    if (li) commit(items.indexOf(li));
  });
}

document.querySelectorAll(".drop > select").forEach(opensUpward);
// Anywhere else, and it is not a choice being made.
window.addEventListener("mousedown", (ev) => {
  if (!ev.target.closest(".drop")) closeDrop();
});
window.addEventListener("keydown", (ev) => openDrop && openDrop.onKey(ev), true);
window.addEventListener("wheel", closeDrop, true);

/// Whether the audio is being rebuilt rather than carried through.
///
/// The two controls under the mode -- channels and bitrate -- only describe
/// an encode, and the other two modes do not run one over the whole track:
/// `copy` runs none at all, and `smart` runs one on two frames per boundary,
/// where the whole point is that they come out the same shape as the frames
/// they are spliced between. So they answer to the mode.
function reencodingAudio() {
  return settings.audio === "reencode";
}

/// Grey out what the chosen mode has no use for.
function lockAudioDetail() {
  el("out-audio-channels").disabled = !reencodingAudio();
  el("out-audio-bitrate").disabled = !reencodingAudio();
}

/// The ladder AAC is spoken in, in bits per second.
const AUDIO_RUNGS = [
  64, 80, 96, 112, 128, 144, 160, 192, 224, 256, 320, 384, 448, 512, 640,
].map((k) => k * 1000);

/// How high the ladder goes, by channel count.
///
/// 384 kbit/s for stereo and 640 for 5.1, which is where a broadcast puts
/// them with room over the top; mono at half the stereo figure. Nothing above
/// 640 is offered at all.
///
/// Worth knowing, though it is not what sets these: the encoder has a ceiling
/// of its own and does not announce it. Asked for more than it can spend,
/// FFmpeg's AAC encoder writes less -- driven with noise at 48 kHz so that it
/// and not the material runs out first, mono walls near 218 kbit/s and stereo
/// near 250. So the top of the stereo ladder is headroom rather than a
/// promise: ask for 384 of stereo and what comes back is what the encoder
/// found worth spending.
const BITRATE_CEILING = { 1: 192_000, 2: 384_000, 6: 640_000 };
const BITRATE_MAX = 640_000;

/// The ceiling for any channel count, named or not.
///
/// The counts the control offers are named. A recording read as 入力と同じ
/// can be any count at all, and one of those takes the ceiling of the next
/// count up -- a 4-channel recording is nearer 5.1 than it is stereo.
function bitrateCap(channels) {
  if (!channels) return BITRATE_MAX;
  const key = Object.keys(BITRATE_CEILING)
    .map(Number)
    .find((n) => n >= channels);
  return key ? BITRATE_CEILING[key] : BITRATE_MAX;
}

/// How many channels the ceiling should be worked out for.
///
/// An explicit choice answers for itself. 入力と同じ does not, and the
/// setting is one answer for a whole list that may hold both a 5.1 recording
/// and a stereo one -- so it is the widest track in the list that decides,
/// which is the widest the ceiling could have to cover. An empty list decides
/// nothing and the whole ladder is offered.
function channelsForCap() {
  if (settings.audioChannels) return Number(settings.audioChannels);
  const counts = ready().map((c) => (c.info && c.info.audio_channels) || 0);
  return counts.length ? Math.max(...counts) : 0;
}

/// Put the rungs worth offering in the bitrate control, and bring the answer
/// it is holding inside them.
function fillBitrates() {
  const select = el("out-audio-bitrate");
  const cap = bitrateCap(channelsForCap());
  const rungs = cap ? AUDIO_RUNGS.filter((b) => b <= cap) : AUDIO_RUNGS;
  // Rebuilt only when it would come out different -- which the language is
  // part of, since おまかせ is a word and not a number.
  const sig = `${currentLang()}|${rungs.length}`;
  if (select.dataset.sig !== sig) {
    select.dataset.sig = sig;
    select.innerHTML =
      `<option value="" data-i18n="bitrate.auto">${esc(t("bitrate.auto"))}</option>` +
      rungs.map((b) => `<option value="${b}">${b / 1000} kbps</option>`).join("");
  }
  // A rate the list no longer offers -- the channel count came down under it,
  // or a project was written by a version whose ladder had other rungs -- is
  // taken to the nearest rung at or below it rather than thrown away.
  const want = Number(settings.audioBitrate) || 0;
  if (want && !rungs.includes(want)) {
    settings.audioBitrate = String(rungs.filter((b) => b <= want).pop() ?? rungs[0]);
  }
  select.value = settings.audioBitrate;
}

/// What the engine will actually be asked for. A control that is greyed out
/// still holds whatever it was last set to -- that is the point of greying it
/// out rather than clearing it -- and what it holds must not reach the cut
/// behind the screen's back.
function audioChannelsOut() {
  return reencodingAudio() && settings.audioChannels ? Number(settings.audioChannels) : null;
}

function audioBitrateOut() {
  return reencodingAudio() && settings.audioBitrate ? Number(settings.audioBitrate) : null;
}

/// What is happening to the audio, when it is not being copied.
///
/// The notes on the output screen are about pictures -- that is what it shows
/// -- and "the whole clip is copied losslessly" stops being true of the file
/// the moment the audio is re-encoded from end to end, which a downmix always
/// is. So the picture's own claim carries this after it.
function audioNote(info) {
  if (!info.has_audio || !reencodingAudio()) return "";
  const from = info.audio_channels || 0;
  const to = audioChannelsOut() || from;
  if (from && to && to !== from) {
    // Which way it goes is the recording's to decide, not the setting's: one
    // list can hold a 5.1 recording and a stereo one, and 2ch asked of both
    // folds the first and spreads the second.
    const key = to < from ? "out.audioDownmixed" : "out.audioUpmixed";
    return " " + t(key, { from: chLabel(from), to: chLabel(to) });
  }
  return " " + t("out.audioReencoded");
}

/// What the output's audio will be, in the one line the format panel has.
function audioSummary(info) {
  if (!info.has_audio) return t("media.audioNo");
  const from = info.audio_channels || 0;
  const to = audioChannelsOut() || from;
  const down = !!(from && to && to !== from);
  const rate = audioBitrateOut();
  const detail = [];
  if (from) detail.push(down ? `${chLabel(from)} → ${chLabel(to)}` : chLabel(from));
  if (rate) detail.push(`${rate / 1000} kbps`);
  const mode = t(`audio.${settings.audio}.short`);
  return detail.length ? t("outset.audioLine", { mode, detail: detail.join(", ") }) : mode;
}

el("browse-dir").addEventListener("click", async (ev) => {
  ev.preventDefault();
  const picked = await dialog.open({ directory: true, multiple: false });
  if (!picked) return;
  settings.dir = picked;
  el("out-dir").value = picked;
  renderOutset();
  renderOutScreen();
  touch();
});

function renderOutset() {
  lockAudioDetail();
  fillBitrates();
  const list = ready();
  const select = el("outset-clip");
  const was = select.value;
  select.innerHTML = list
    .map((c, i) => `<option value="${c.id}">${i + 1}: ${esc(clipLabel(c))}</option>`)
    .join("");
  if (list.some((c) => String(c.id) === was)) select.value = was;
  const clip = byId(Number(select.value)) || list[0];
  const box = el("outset-format");
  if (!clip) {
    box.textContent = t("outset.noReady");
    return;
  }
  const i = clip.info;
  const keeps = keepsOf(clip);
  const kept = keeps.reduce((n, k) => n + (k.b - k.a), 0);
  box.textContent = t("outset.format", {
    codec: i.codec,
    w: i.width,
    h: i.height,
    fps: i.fps.toFixed(2),
    scan: t(i.interlaced ? "outset.interlaced" : "media.progressive"),
    audio: audioSummary(i),
    keeps: keeps.length,
    kept: fmt(kept),
    dur: fmt(i.duration),
    cuts: clip.edit ? clip.edit.cuts.length : 0,
    out: outputPath(clip),
    side:
      settings.keyframes && clip.edit && clip.edit.keyframes.length
        ? t("outset.sidecar", {
            path: `${outputPath(clip).replace(/\.[^./\\]*$/, "")}.keyframe`,
          })
        : "",
  });
}
el("outset-clip").addEventListener("change", renderOutset);

// --- what will actually be re-encoded -------------------------------------
//
// The output screen's picture. A smart render copies the recording bit for
// bit apart from the part-GOPs a cut lands inside, so these few frames are
// the whole of what this program can be blamed for; showing them is showing
// the only thing on the screen worth looking at.
//
// One at a time, on the stage, following the write. There is no strip of
// them: this screen is watched while it works rather than worked in, and the
// frame the head is passing through is the one being asked about.

/// The plan's re-encoded segments for `clip`, with a frame out of each.
///
/// Cached against the cuts they were worked out for, because the plan is a
/// read of the recording's leading pictures and the frames are decodes --
/// neither worth repeating every time the screen is drawn.
async function reencodeOf(clip) {
  const ranges = rangesOf(clip);
  const sig = JSON.stringify(ranges);
  if (clip.reencode && clip.reencode.sig === sig) return clip.reencode;
  const plan = await invoke("clip_plan", { path: clip.path, ranges });
  const segs = plan.segments.filter((g) => g.kind !== "copy");
  // The middle of the segment rather than its start: the start is the join
  // itself, and what you want to see is the picture the encoder had to make.
  const shots = segs.length
    ? await invoke("clip_thumbs", {
        path: clip.path,
        times: segs.map((g) => (g.start + g.end) / 2),
        width: 480,
      })
    : [];
  // Where each one falls in the finished file. `out` is what the stage
  // prints -- this screen is about the file being written, so its clock is
  // the one to show -- and `at` the same thing as a fraction, which is what
  // the progress reports come in as.
  const keeps = keepsOf(clip);
  const outDur = keeps.reduce((n, k) => n + (k.b - k.a), 0) || 1;
  const out = segs.map((g) => srcToOut(keeps, g.start) ?? 0);
  const at = out.map((o) => o / outDur);
  clip.reencode = { sig, plan, segs, shots, out, at };
  return clip.reencode;
}

/// What the stage is currently speaking for: the clip, its re-encoded
/// segments, and which of them is up. `note` is the picture half of the line
/// under it -- see `paintShotsNote`.
let onShow = null;

/// Put the line under the stage up, picture half and audio half.
///
/// The picture half is worked out once and cached with the frames, because
/// getting it costs a plan and some decodes. The audio half is a reading of
/// the output settings, which can change while this screen is up -- and does,
/// since every settings change repaints it -- so it is composed here rather
/// than baked into what the cache holds. A note saying the audio is being
/// re-encoded when the mode has since gone back to smart rendering is a lie
/// about the file that is about to be written.
function paintShotsNote() {
  if (!onShow || !onShow.note) return;
  const box = el("out-shots-note");
  box.className = onShow.note.className;
  box.textContent = onShow.note.text + audioNote(onShow.clip.info);
}
/// Keyed on the clip *and its cuts*, so coming back after changing one looks
/// at the new joins rather than the ones that were there before.
let shownReencode = null;
let shotsToken = 0;

async function showReencode(clip) {
  const token = ++shotsToken;
  if (!clip) {
    onShow = shownReencode = null;
    el("out-shots-note").textContent = "";
    stageShot(null);
    return;
  }
  const key = JSON.stringify([clip.id, rangesOf(clip)]);
  if (shownReencode === key) return;
  shownReencode = key;
  el("out-shots-note").className = "grow dim";
  el("out-shots-note").textContent = t("out.looking");
  // Nothing to repaint until there is an answer: this runs on into an await,
  // and a `paintShotsNote` in the meantime would put the last clip's line
  // back over "working it out".
  if (onShow) onShow.note = null;
  stageShot(null, t("out.lookingAt", { clip: clipLabel(clip) }));
  try {
    const r = await reencodeOf(clip);
    if (token !== shotsToken) return;
    onShow = { clip, r, at: -1, note: null };
    const redone = r.segs.reduce((n, g) => n + g.frames, 0);
    if (!r.segs.length) {
      // Cuts that all landed on access points, or no cuts at all. Worth
      // saying rather than leaving it blank: it is the best outcome this
      // program has.
      onShow.note = {
        className: "grow lossless",
        text: t("out.losslessNote", { clip: clipLabel(clip) }),
      };
      paintShotsNote();
      // The clip's own poster rather than a black rectangle. It does not
      // contradict what this screen is for: the line under it says there is
      // nothing to re-encode, so the picture is standing for the clip about
      // to be written and not for a frame being made again.
      stageShot(null, t("out.losslessStage"), clip.info.poster);
      return;
    }
    onShow.note = {
      className: "grow dim",
      text: t("out.shots", { clip: clipLabel(clip), n: r.segs.length, frames: redone }),
    };
    paintShotsNote();
    stageShot(0);
  } catch (e) {
    if (token !== shotsToken) return;
    shownReencode = null;
    el("out-shots-note").className = "grow dim";
    el("out-shots-note").textContent = t("out.cannotLook", { e });
  }
}

/// Put segment `i` on the stage.
///
/// `note` stands in for the sub-line when there is no segment to show, and
/// `poster` for the picture -- the fully lossless case, which is worth saying
/// rather than leaving blank.
function stageShot(i, note = "", poster = null) {
  const img = el("out-preview");
  if (i === null || !onShow || !onShow.r.segs.length) {
    // Hidden rather than left with no `src`, which draws as a broken picture.
    if (poster) img.src = poster;
    else img.removeAttribute("src");
    img.hidden = !poster;
    // No frame on the stage, so no frame number or timecode to put under it.
    el("out-ovl-main").hidden = true;
    el("out-ovl-frame").textContent = "—";
    el("out-ovl-time").textContent = "--:--:--.--";
    el("out-ovl-kind").textContent = note || "—";
    el("out-ovl-note").textContent = "";
    return;
  }
  el("out-ovl-main").hidden = false;
  const { r, clip } = onShow;
  i = clamp(i, 0, r.segs.length - 1);
  onShow.at = i;
  const g = r.segs[i];
  const shot = r.shots[i];
  img.hidden = !shot;
  if (shot) img.src = shot.url;
  else img.removeAttribute("src");
  el("out-ovl-frame").textContent = String(Math.round(r.out[i] * clip.info.fps));
  el("out-ovl-time").textContent = fmt(r.out[i]);
  el("out-ovl-kind").textContent = t("out.ovlKind", { i: i + 1, n: r.segs.length });
  el("out-ovl-note").textContent = t("out.ovlNote", { n: g.frames });
}

/// Follow the writing head: put the segment it is passing through on the
/// stage.
function followWrite(done) {
  if (!onShow || !onShow.r.segs.length) return;
  const { r } = onShow;
  let at = -1;
  for (let i = 0; i < r.at.length; i++) if (done >= r.at[i] - 0.001) at = i;
  if (at >= 0 && at !== onShow.at) stageShot(at);
}

// --- output -------------------------------------------------------------

let exporting = false;
let abort = false;
/// The row being written, so a progress event can be told apart from a stale
/// one belonging to the row before it.
let writing = null;
let began = 0;

function renderOutScreen() {
  el("out-dir-shown").value = settings.dir || t("outset.sameAsInput");
  const list = ready();
  el("out-idle").hidden = list.length > 0;
  // Idle, the screen speaks for whichever clip is about to be written first;
  // running, `runExport` points it at the one under the head.
  if (!exporting) showReencode(list[0] || null);
  // The picture half of the note is cached against the clip and its cuts;
  // this puts the audio half back on it, which the settings can have changed
  // since.
  paintShotsNote();
  el("out-list").innerHTML = list
    .map((c, i) => {
      const kept = keepsOf(c).reduce((n, k) => n + (k.b - k.a), 0);
      return `<li class="${c.out.state}">
        <span class="n">${i + 1}</span>
        <span class="nm">${esc(clipLabel(c))}</span>
        <span class="len dim">${fmt(kept)}</span>
        <span class="pbar"><span style="width:${Math.round(c.out.progress * 100)}%"></span></span>
        <span class="note dim">${esc(c.out.note || "")}</span>
      </li>`;
    })
    .join("");
  paintButtons();
}

function paintOutProgress(overall) {
  const pct = Math.round(overall * 100);
  el("progress-bar").style.width = `${pct}%`;
  el("out-pct").textContent = `${pct}%`;
  const spent = (Date.now() - began) / 1000;
  el("out-elapsed").textContent = t("out.elapsed", { t: clock(spent) });
  el("out-left").textContent =
    overall > 0.01
      ? t("out.left", { t: clock((spent / overall) * (1 - overall)) })
      : t("out.leftUnknown");
}

if (listen) {
  listen("export-progress", (ev) => {
    const [path, done] = ev.payload;
    if (!writing || writing.path !== path) return;
    const clip = writing;
    clip.out.progress = done;
    clip.out.note = `${Math.round(done * 100)}%`;
    followWrite(done);
    renderOutScreen();
    const all = ready();
    const finished = all.filter((c) => c.out.state === "done").length;
    paintOutProgress(all.length ? (finished + done) / all.length : 0);
  });
}

el("abort-export").addEventListener("click", () => {
  abort = true;
  el("out-state").textContent = t("out.aborting");
});

el("run-export").addEventListener("click", runExport);

async function runExport() {
  if (exporting) return;
  // Nothing to collect from the editor first: it reports every change as it
  // makes it, so what the list holds is already what is on screen in there.
  const list = ready();
  if (!list.length) return;
  // A pass over another recording would be competing for the same disc, and
  // unlike the editor this is work with an end in sight that somebody is
  // watching. Both lanes stand aside until the list is written out.
  paused = true;
  await invoke("stop_batch", { lane: null });

  exporting = true;
  abort = false;
  began = Date.now();
  list.forEach((c) => (c.out = { state: "waiting", progress: 0, note: t("out.waiting") }));
  el("abort-export").disabled = false;
  paintButtons();
  renderOutScreen();

  let done = 0;
  for (const clip of list) {
    if (abort) {
      clip.out = { state: "skipped", progress: 0, note: t("out.skipped") };
      continue;
    }
    const out = outputPath(clip);
    if (out === clip.path) {
      clip.out = { state: "error", progress: 0, note: t("out.sameName") };
      renderOutScreen();
      continue;
    }
    writing = clip;
    clip.out = { state: "running", progress: 0, note: "0%" };
    el("out-state").textContent = t("out.writing", { name: nameOf(out) });
    renderOutScreen();
    // Before the cut starts, not during: the plan and the frames are reads of
    // the same recording the cut is about to stream off the disc.
    await showReencode(clip);
    // A second run over a clip already on show would otherwise start from
    // wherever the first one left the stage.
    if (onShow && onShow.r.segs.length) stageShot(0);
    try {
      await invoke("export", {
        path: clip.path,
        ranges: rangesOf(clip),
        output: out,
        audioCopy: settings.audio === "copy",
        audioReencode: settings.audio === "reencode",
        audioChannels: audioChannelsOut(),
        audioBitrate: audioBitrateOut(),
        // What the editor's track menu switched off for this clip. Per clip
        // and not per list: the audio settings above are one answer for the
        // whole run, but which of a recording's own streams are wanted is a
        // fact about that recording.
        dropStreams: clip.edit ? clip.edit.dropStreams || [] : [],
      });
      let extra = "";
      if (settings.keyframes) {
        // Numbered against the file being written, not the recording.
        const keeps = keepsOf(clip);
        const frames = (clip.edit ? clip.edit.keyframes : [])
          .map((t) => srcToOut(keeps, t))
          .filter((o) => o !== null)
          .map((o) => Math.round(o * clip.info.fps));
        // A clip with no marks gets no sidecar. The setting is on for the
        // whole list, and most lists have clips nobody put a mark in; an
        // empty `.keyframe` beside them says "there are no marks here", which
        // is exactly what no file at all already says, and it is one more
        // file to notice and delete.
        if (frames.length) {
          const side = out.replace(/\.[^./\\]*$/, "") + ".keyframe";
          const n = await invoke("write_keyframes", { path: side, frames, fps: clip.info.fps });
          extra = t("out.doneKeyframes", { n });
        }
      }
      clip.out = { state: "done", progress: 1, note: t("out.done", { extra }) };
      done++;
    } catch (e) {
      clip.out = { state: "error", progress: 0, note: String(e) };
    }
    writing = null;
    renderOutScreen();
    paintOutProgress(done / list.length);
  }

  exporting = false;
  writing = null;
  el("abort-export").disabled = true;
  const failed = list.filter((c) => c.out.state === "error").length;
  el("out-state").textContent = t("out.summary", {
    done,
    all: list.length,
    failed: failed ? t("out.summaryFailed", { n: failed }) : "",
    aborted: abort ? t("out.summaryAborted") : "",
    elapsed: clock((Date.now() - began) / 1000),
  });
  paintOutProgress(1);
  paused = false;
  pump();
  renderOutScreen();
}

// --- プロジェクト ---------------------------------------------------------
//
// An evening's work is a list of recordings, what has been cut out of each of
// them, and where the results are to go. None of that is on disc: close the
// program and it is gone. That is no loss for one clip in one sitting, and a
// real one for twenty over a weekend -- so it can be written down and picked
// up again.
//
// **Only what could not be worked out again is written.** A recording's
// length, shape and frame rate come back with its seek index; the index and
// the commercial detections are already cached beside the recording by the
// backend. So the file holds paths, cuts, marks and the output settings, and
// opening it re-reads the list exactly as adding the same files would --
// which also means a project opened on another machine, or after the caches
// have been cleared, is a project that still opens. It simply reads again.
//
// It does not hold the pictures, the plan or anything else the screens work
// out for themselves, and it never will: a project that carried a copy of
// what is on disc would be a project that could disagree with it.

const PROJECT_EXT = "scproj";

/// The format's own number, which is not the program's. It goes up when a
/// file written by an older version would be read *wrongly* rather than
/// merely incompletely -- a field added is not a new format, since a reader
/// that has never heard of it leaves it alone.
const PROJECT_VERSION = 1;

/// The file this list is currently in, or "" for work that has never been
/// saved. What 保存 writes over without asking.
let projectPath = "";

/// Everything worth keeping, in the shape it goes on disc.
function captureProject() {
  return {
    smartcut: PROJECT_VERSION,
    saved: new Date().toISOString(),
    settings: { ...settings },
    clips: clips.map((c) => ({
      path: c.path,
      // What the editor handed back the last time this row was in it: the
      // cuts, the marks, and where the playhead was left. Null for a row
      // nobody has opened yet, which is not the same as a row cut to nothing.
      edit: c.edit,
      // Blocks a detection found that the timeline has not been shown yet.
      // The blocks are not written -- they are beside the recording -- but
      // whether they are still owed to the editor is this list's own
      // knowledge, and the cache cannot answer it.
      cmPending: c.cmPending,
    })),
  };
}

/// Where the picker opens for a list that has no name yet: beside the output
/// if one has been chosen, and otherwise beside the recordings, because that
/// is where the work is.
function defaultProjectPath() {
  const dir = settings.dir
    ? settings.dir.replace(/[/\\]*$/, "/")
    : dirOf(clips[0] ? clips[0].path : "");
  return `${dir}${t("project.untitled")}.${PROJECT_EXT}`;
}

/// What the project would be if it were written this instant, as one string.
///
/// The saved timestamp is left out -- it changes every time and says nothing
/// about the work -- and so is `cmPending`, which is written to the file but
/// is not something that can be *lost*: a detection is cached beside the
/// recording, and opening the project again works the flag out from it.
function shapeOf() {
  return JSON.stringify({
    settings,
    clips: clips.map((c) => ({ path: c.path, edit: c.edit })),
  });
}

/// The shape the file on disc has. Set when one is written or read, and
/// compared against rather than raised as a flag: a flag has to be lowered
/// again by everything that puts the work back where it was -- cancelling
/// out of the editor, a clip added and removed -- and the one place that
/// forgets leaves a program insisting there is something to lose when there
/// is not.
let savedShape = shapeOf();

/// Whether there is work here that is not on disc.
///
/// An empty list with no project open is nothing to lose, whatever it held
/// a moment ago: there is no work in an empty list, and there is no file it
/// belongs to. Without that, emptying a list left a `*` in the title that
/// nothing could clear -- there was nothing to save, so saving could not
/// clear it.
const dirty = () => (!clips.length && !projectPath ? false : shapeOf() !== savedShape);

/// The last title and the last answer sent down, so that neither is sent
/// twice: this runs after every repaint, and most repaints change neither.
let shownTitle = null;
let shownDirty = null;

/// The list window's title bar. The only place the open project is readable
/// without opening a menu, and the reason the title bar is worth writing to
/// at all -- two SmartCut windows on a taskbar are otherwise the same word
/// twice. A `*` in front is work that is not on disc.
///
/// An empty list that has never been saved is not called 無題: there is
/// nothing there to be a draft of, and the program's own name is the honest
/// thing to have in the corner.
function retitleMain() {
  if (!invoke) return;
  const unsaved = dirty();
  const title =
    !projectPath && !clips.length && !unsaved
      ? "SmartCut"
      : t("project.windowTitle", {
          mark: unsaved ? "*" : "",
          name: projectPath ? nameOf(projectPath) : t("project.untitled"),
        });
  if (title !== shownTitle) {
    shownTitle = title;
    invoke("retitle_main", { title });
  }
  // The window must not close on work that is not on disc, and stopping it
  // is the one thing this side cannot do for itself.
  if (unsaved !== shownDirty) {
    shownDirty = unsaved;
    invoke("set_dirty", { dirty: unsaved });
  }
}

/// Say that something that goes into a project may have changed.
///
/// Called from the two places everything funnels through -- the repaint of
/// the whole list, and the editor reporting a cut -- rather than from each
/// of the dozen things that can change it. Working the answer out is a
/// `JSON.stringify` of a few hundred bytes; remembering to raise a flag in
/// twelve places is a bug waiting for the thirteenth.
function touch() {
  retitleMain();
}

async function writeProject(path) {
  try {
    // Indented, and with the paths first in every row: a project is a plain
    // file about files, and someone who opens one in an editor to see which
    // recordings it names should be able to read it.
    await invoke("write_project", {
      path,
      body: JSON.stringify(captureProject(), null, 2),
    });
  } catch (e) {
    note(`${e}`);
    return false;
  }
  projectPath = path;
  savedShape = shapeOf();
  retitleMain();
  note(t("project.saved", { name: nameOf(path) }));
  return true;
}

/// 保存 and 名前を付けて保存, which differ only in whether the name is
/// already settled.
///
/// An empty list that has never been saved has nothing to write down. An
/// empty list that *is* a project is another matter -- emptying one is a
/// change like any other, and it has to be recordable or the `*` it puts in
/// the title could never be cleared.
async function saveProject(rename = false) {
  if (!clips.length && !projectPath) {
    note(t("project.nothingToSave"));
    return false;
  }
  if (!rename && projectPath) return writeProject(projectPath);
  const picked = await dialog.save({
    defaultPath: projectPath || defaultProjectPath(),
    filters: [{ name: t("dialog.project"), extensions: [PROJECT_EXT] }],
  });
  if (!picked) return false;
  // The picker hands back what was typed, and what was typed is often a name
  // without an extension -- which would make a file its own filter would not
  // show again. So it is put on here rather than trusted to the dialog.
  return writeProject(extOf(picked) === PROJECT_EXT ? picked : `${picked}.${PROJECT_EXT}`);
}

/// Whether it is all right to put the current list down.
///
/// Only over work that is not on disc. A project opened, looked at and
/// closed again has nothing to lose, and a dialog that comes up anyway is a
/// dialog that gets dismissed without being read.
async function askReplace() {
  if (!dirty()) return true;
  return dialog.ask(t("project.replaceBody"), {
    title: t("project.replaceTitle"),
    kind: "warning",
  });
}

async function openProject() {
  // Asked before the picker rather than after it: the question is whether to
  // put this list down, and someone who answers no has been spared choosing
  // a file for nothing.
  if (!(await askReplace())) return;
  const picked = await dialog.open({
    multiple: false,
    filters: [{ name: t("dialog.project"), extensions: [PROJECT_EXT] }],
  });
  if (!picked) return;
  await loadProject(Array.isArray(picked) ? picked[0] : picked);
}

/// Put a project file's list up, in place of whatever is there.
///
/// A recording the file names that is no longer where it was is not stopped
/// on: the row goes up like any other and the index pass says what happened
/// to it, in the row itself, where it can be looked at next to the ones that
/// were fine. Refusing the whole project over one moved file would be the
/// worse trade -- the other nineteen rows are still exactly right.
async function loadProject(path) {
  let doc;
  try {
    doc = JSON.parse(await invoke("read_project", { path }));
  } catch (e) {
    note(t("project.cannotOpen", { name: nameOf(path), e }));
    return;
  }
  // A number this program has never heard of is a file from a later one, and
  // what it would lose on the way in is exactly the part it does not
  // recognise. Dropping somebody's cuts quietly is worse than not opening.
  if (!doc || typeof doc.smartcut !== "number" || doc.smartcut > PROJECT_VERSION) {
    note(t("project.wrongFormat", { name: nameOf(path) }));
    return;
  }
  // Not merely emptied: a lane reading a row has to be told to stop, and the
  // editor open on one has nothing left to be about. All of which `remove`
  // already knows how to do.
  await remove(clips.slice());
  // Key by key rather than wholesale, so that a file cannot put anything in
  // `settings` that the output screen has no control for.
  for (const key of Object.keys(settings)) {
    if (doc.settings && key in doc.settings) settings[key] = doc.settings[key];
  }
  showSettings();
  const taken = [];
  for (const saved of Array.isArray(doc.clips) ? doc.clips : []) {
    if (!saved || typeof saved.path !== "string") continue;
    const clip = makeClip(saved.path);
    // The row's id is this session's counting, so the saved edit is
    // readdressed to the row it has just become. Everything else in it is
    // source time and travels unchanged.
    if (saved.edit) clip.edit = { ...saved.edit, id: clip.id, path: clip.path };
    clips.push(clip);
    taken.push([clip, !!saved.cmPending]);
  }
  projectPath = path;
  savedShape = shapeOf();
  retitleMain();
  show("input");
  renderList();
  note(t("project.opened", { name: nameOf(path), n: taken.length }));
  (async () => {
    for (const [clip, pending] of taken) await restoreCm(clip, pending);
  })();
  pump();
}

// The window's cross, over work that is not on disc. Rust holds the close
// while this is asked -- a page cannot stop its own window from going away --
// and lets it through only when `quit` is called. Cancelling answers it by
// doing nothing at all, which is why nothing here is remembered about having
// been asked.
if (listen) {
  listen("close-requested", async () => {
    const go = await dialog.ask(t("project.quitBody"), {
      title: t("project.quitTitle"),
      kind: "warning",
      okLabel: t("project.quitOk"),
      cancelLabel: t("project.quitCancel"),
    });
    if (go) invoke("quit");
  });
}

/// A project arriving by drag and drop, which skips the picker but not the
/// question the picker's caller asks first.
async function openDroppedProject(path) {
  if (!(await askReplace())) return;
  await loadProject(path);
}

// The three items the menu carries about the work rather than about the
// program. `showMenu(false)` first in each: the file picker is a window of
// its own, and a menu left standing behind it is still there when it closes.
el("menu-open").addEventListener("click", () => {
  showMenu(false);
  openProject();
});
el("menu-save").addEventListener("click", () => {
  showMenu(false);
  saveProject();
});
el("menu-save-as").addEventListener("click", () => {
  showMenu(false);
  saveProject(true);
});

// Ctrl+S, Ctrl+Shift+S and Ctrl+O, on every screen rather than only on the
// list: they are about the program's work as a whole, and the output
// settings are as much a part of a project as the cuts are. Kept out of the
// list's own key handler for that reason.
window.addEventListener("keydown", (ev) => {
  if (!(ev.ctrlKey || ev.metaKey) || ev.altKey) return;
  const key = ev.key.toLowerCase();
  if (key === "s") {
    ev.preventDefault();
    saveProject(ev.shiftKey);
  } else if (key === "o" && !ev.shiftKey) {
    ev.preventDefault();
    openProject();
  }
});

// --- the program's own menu ----------------------------------------------
//
// One button in the corner and one item under it. It is not a menu bar and
// should not grow into one: everything about the *clips* is on the screens,
// and this is for the few things that are about the program.

const brand = el("brand");
const brandMenu = el("brand-menu");

function showMenu(on) {
  brandMenu.hidden = !on;
  brand.setAttribute("aria-expanded", String(!!on));
  brand.classList.toggle("open", !!on);
}

brand.addEventListener("click", (ev) => {
  ev.stopPropagation();
  showMenu(brandMenu.hidden);
});
// Anywhere else, and Escape: a menu left standing over the list is a menu
// that has to be dismissed before anything can be clicked, and the button
// that opened it is not always the one the eye goes back to.
window.addEventListener("click", () => showMenu(false));
window.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") showMenu(false);
});

// --- 環境設定 -------------------------------------------------------------

const prefs = el("prefs");

function showPrefs(on) {
  prefs.hidden = !on;
  if (on) el("pref-lang").value = preference();
}

el("menu-prefs").addEventListener("click", () => {
  showMenu(false);
  showPrefs(true);
});
el("prefs-close").addEventListener("click", () => showPrefs(false));
// The dark ground behind the panel, but not the panel itself.
prefs.addEventListener("click", (ev) => {
  if (ev.target === prefs) showPrefs(false);
});
window.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape" && !prefs.hidden) showPrefs(false);
});

el("pref-lang").addEventListener("change", async (ev) => {
  setLang(ev.target.value);
  // Going back to "follow the machine" has to ask the machine again, and the
  // webview's own answer is not it: WebKitGTK reports the browser's idea of
  // a preferred language, which on a Japanese desktop is still en-US. Same
  // correction as at startup, and for the same reason.
  await confirmWithOs(invoke);
  await tellBackend(invoke);
  // The cut editor is a window of its own with its own copy of the
  // catalogue, and it does not read the store again while it is up.
  // The language it resolved to, not the preference: "auto" is answered
  // from the webview's own idea of the machine, and this window may already
  // have been corrected by the backend's.
  if (emit) emit("lang-changed", currentLang());
});

// --- バージョン情報 --------------------------------------------------------
//
// The other panel under the name in the corner. What it shows is asked of
// the backend, which is the only side that knows any of it: the version is
// stamped into the binary, and the libav numbers belong to the libraries
// this process loaded rather than the ones it was written against.
//
// Asked once and kept. None of it can change while the program is running,
// and a panel that has to wait for a round trip before it says anything is a
// panel that opens empty.

const about = el("about");

/// What the backend said, or nothing until it has been asked.
let versions = null;

/// Put the answer on the panel. Called again on a language change, because
/// three of these lines are sentences rather than values.
function paintAbout() {
  const unknown = t("about.unknown");
  const v = versions;
  el("about-version").textContent = t("about.version", { v: v ? v.app : unknown });
  el("about-core").textContent = v ? v.core : unknown;
  el("about-libav").textContent = v
    ? t("about.libav", { f: v.avformat, c: v.avcodec, u: v.avutil })
    : unknown;
  el("about-libav-license").textContent = v ? v.ffmpeg_license : unknown;
  el("about-platform").textContent = v ? v.platform : unknown;
}

async function showAbout(on) {
  about.hidden = !on;
  if (!on) return;
  // Whatever is known now, so the panel is never blank; then the answer,
  // which on every open after the first is already in hand.
  paintAbout();
  if (versions || !invoke) return;
  try {
    versions = await invoke("versions");
  } catch (e) {
    // An older backend without the command. 不明 on every line is a truthful
    // answer and a legible one; there is nothing here worth an error for.
    jlog(`versions ${e}`);
    return;
  }
  paintAbout();
}

el("menu-about").addEventListener("click", () => {
  showMenu(false);
  showAbout(true);
});
el("about-close").addEventListener("click", () => showAbout(false));
about.addEventListener("click", (ev) => {
  if (ev.target === about) showAbout(false);
});
window.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape" && !about.hidden) showAbout(false);
});

/// Say everything this window has already said, in the language now in
/// force.
///
/// `applyStatic` has done the markup by the time this runs; what is left is
/// everything built out of `t` at the moment it was shown. Most of it is
/// simply redrawn. The sentences that were *stored* rather than drawn --
/// what a commercial detection found, how a clip's index was come by -- are
/// worked out again from what they were worked out from, which is why the
/// row remembers where its note came from. A note the editor wrote is left
/// alone: this window does not hold what it was made of.
function relocalise() {
  for (const c of clips) {
    if (c.state === "ready" && c.info) {
      c.phase = c.info.cached
        ? t("phase.indexReused")
        : t("phase.indexBuilt", { s: c.info.seconds.toFixed(0) });
    }
    if (c.cmState === "done" && c.cm && c.cmSource) {
      const note = cmNote(c.cm);
      c.cmPhase = c.cmSource === "cache" ? t("cm.previous", { note }) : note;
    }
  }
  renderList();
  renderOutset();
  renderOutScreen();
  paintQueueNote();
  paintAbout();
  // The editor's window title is this window's doing -- it names the clip,
  // which only the list knows how to name -- so it is this window that has to
  // put it right.
  if (editing) {
    invoke("retitle_editor", { title: t("editor.windowTitle", { clip: clipLabel(editing) }) });
  }
}
onLangChange(relocalise);

// --- keys on the list ---------------------------------------------------
//
// Only while the list is the screen on show. The editor has its own key
// handler, and Ctrl+D means the same thing on both screens for one clip and
// for many.

window.addEventListener("keydown", (ev) => {
  if (screen !== "input") return;
  // A panel is over the list: the ground behind it says the rest of the
  // program is not listening, and Delete deleting a clip out from under it
  // would be the list listening anyway.
  if (!prefs.hidden || !about.hidden) return;
  if (ev.target.tagName === "INPUT" || ev.target.tagName === "SELECT") return;
  const key = ev.key.toLowerCase();
  if ((ev.ctrlKey || ev.metaKey) && key === "a") {
    ev.preventDefault();
    selectAll();
    return;
  }
  if ((ev.ctrlKey || ev.metaKey) && key === "d") {
    ev.preventDefault();
    detectSelected();
    return;
  }
  if (ev.ctrlKey || ev.metaKey || ev.altKey) return;
  if (ev.key === "Delete" || ev.key === "Backspace") {
    ev.preventDefault();
    remove(selected());
    return;
  }
  if (ev.key === "Enter") {
    ev.preventDefault();
    const one = selected();
    if (one.length === 1) edit(one[0]);
    return;
  }
  if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
    ev.preventDefault();
    if (!clips.length) return;
    const step = ev.key === "ArrowDown" ? 1 : -1;
    const at = clamp((anchor < 0 ? (step > 0 ? -1 : clips.length) : anchor) + step, 0, clips.length - 1);
    pick(clips[at], { shiftKey: ev.shiftKey });
    clips[at].row.scrollIntoView({ block: "nearest" });
  }
});

// --- start --------------------------------------------------------------

jlog("app wired");
noBrowserMenu();
applyStatic();
el("pref-lang").value = preference();
// The three readouts on the output screen that stand at rest until something
// is written. Set here rather than marked up, so that a language change
// during a run does not blank a summary that has just been printed.
el("out-state").textContent = t("out.waiting");
el("out-elapsed").textContent = t("out.elapsed", { t: clock(0) });
el("out-left").textContent = t("out.leftUnknown");
renderList();
show("input");
// The backend writes its own sentences -- the phases under a progress bar,
// and what comes back when a recording will not open -- so it is told the
// language before anything is asked of it, and told again if the machine
// turns out to disagree with what the webview said it was set to.
tellBackend(invoke)
  .then(() => confirmWithOs(invoke))
  .then((changed) => (changed ? tellBackend(invoke) : null))
  .then(() => invoke("initial_paths"))
  .then(async (paths) => {
    if (!paths || !paths.length) return;
    // Launched on files, from a file manager or the command line. They go
    // into the list like any others; a single one goes straight on into the
    // editor, which is what happened before there was a list. Several do
    // not -- being handed a batch is a reason to be shown the batch.
    jlog(`initial_paths -> ${paths.join(", ")}`);
    // Launched on a project rather than on recordings -- from the command
    // line, or from a file manager that has been told what a .scproj is.
    // Nothing to ask about: the list it is replacing is empty.
    if (paths.length === 1 && extOf(paths[0]) === PROJECT_EXT) {
      await loadProject(paths[0]);
      return;
    }
    const taken = await addPaths(paths);
    // The list as the command line handed it over is not work anybody did:
    // it is how the program was started, and starting it the same way again
    // would give the same list back. So it is what the title compares
    // against, and a program launched on a folder does not open with a `*`
    // over a list nobody has touched.
    savedShape = shapeOf();
    retitleMain();
    const one = taken.length === 1 && taken[0];
    if (!one) return;
    one.selected = true;
    paintList();
    // Straight in, without waiting for the index: the editor builds what it
    // needs itself and shows the recording as it goes. Waiting was for when
    // the list had to have the disc to itself.
    edit(one);
  })
  .catch((e) => jlog(`initial_paths: ${e}`));
