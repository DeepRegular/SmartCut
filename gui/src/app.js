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

import { fmt, clock, coarse, cmNote, esc, noBrowserMenu } from "./shared.js";

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
  return n ? `${clip.name}（${n}）` : clip.name;
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
    phase: "待機中",
    progress: 0,
    error: "",
    cm: null, // the last CmResult for this clip
    cmState: "none", // none | queued | running | done | error
    cmPhase: "",
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
    note(`${clipLabel(clip)} は読み込めませんでした`);
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
    await invoke("open_editor", { title: `カット編集 — ${clipLabel(clip)}` });
    // Lost if the window is still starting up, which is what `editor-ready`
    // is for; sent anyway for the case where it is already open on another
    // clip and there will be no `editor-ready` at all.
    tellEditor();
  } catch (e) {
    note(`編集画面を開けません: ${e}`);
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
    note(failed[0].error + (failed.length > 1 ? ` ほか ${failed.length - 1} 件` : ""));
  } else if (skipped.length) {
    note(`対応していない形式のため無視しました: ${skipped.slice(0, 3).join(", ")}` +
      (skipped.length > 3 ? ` ほか ${skipped.length - 3} 件` : ""));
  }
  if (taken.length) pump();
  return taken;
}

el("add-files").addEventListener("click", async () => {
  const picked = await dialog.open({
    multiple: true,
    filters: [{ name: "動画", extensions: VIDEO_EXT }],
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
  if (ix) bits.push(`シーク用インデックスを作成中: ${clipLabel(ix)}`);
  if (cm) bits.push(`CM を検出中: ${clipLabel(cm)}`);
  el("queue-note").textContent = bits.length ? bits.join("　／　") : sticky;
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
  clip.phase = "読み込み中";
  clip.progress = 0;
  paintRow(clip);
  paintQueueNote();
  try {
    clip.info = await invoke("index_clip", { path: clip.path });
    clip.state = "ready";
    clip.progress = 1;
    clip.phase = clip.info.cached ? "前回の索引を再利用" : `索引 ${clip.info.seconds.toFixed(0)} 秒`;
  } catch (e) {
    if (String(e).includes("cancelled")) {
      // Put back, not failed: 中止 means "not now", and the pass left
      // nothing behind to be inconsistent about.
      clip.state = "queued";
      clip.phase = "中止しました";
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
async function restoreCm(clip) {
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
  clip.cmPhase = `${cmNote(res)}（前回の検出）`;
  clip.cmPending = res.blocks.length > 0;
  paintRow(clip);
  paintButtons();
  if (clip.selected) paintProps();
}

async function runCm(clip) {
  clip.cmState = "running";
  clip.cmProgress = 0;
  clip.cmPhase = "検出中";
  paintRow(clip);
  paintQueueNote();
  try {
    const res = await invoke("detect_cm_at", { path: clip.path });
    clip.cm = res;
    clip.cmState = "done";
    clip.cmPhase = cmNote(res);
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
      clip.cmPhase = "中止しました";
    } else {
      clip.cmState = "error";
      clip.cmPhase = `検出できません: ${e}`;
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
      <button class="kill" title="この行を一覧から外す">×</button>`;
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
      ? `${coarse(i.duration)} (${i.frames} フレーム)　00:00:00.00-${fmt(i.duration)}　` +
        `${i.width}x${i.height}　${i.fps.toFixed(2)} fps　${i.codec}` +
        `${i.has_audio ? "" : "　音声なし"}`
      : clip.state === "error"
        ? clip.error
        : clip.path
  );

  // Two things worth saying about a clip below its name: what the detection
  // found, and how much of it the edit takes out. Both are about the clip and
  // neither is about the file, which is what the line above is for.
  const bits = [];
  if (clip.cmState === "running") {
    bits.push(`CM 検出中 ${Math.round(clip.cmProgress * 100)}% — ${clip.cmPhase}`);
  } else if (clip.cmState === "queued") bits.push("CM 検出 待機中");
  else if (clip.cmPhase) bits.push(`CM: ${clip.cmPhase}`);
  const cutCount = clip.edit ? clip.edit.cuts.length : 0;
  if (cutCount && i) {
    const kept = keepsOf(clip).reduce((n, k) => n + (k.b - k.a), 0);
    bits.push(`カット ${cutCount} 箇所 — 出力 ${fmt(kept)}`);
  }
  if (clip.edit && clip.edit.keyframes.length) {
    bits.push(`キーフレーム ${clip.edit.keyframes.length}`);
  }
  setText(li.querySelector(".cm"), bits.join("　／　"));

  // Being edited is worth saying over anything else the row could say: it
  // is the one state that is about where the clip is rather than what has
  // been worked out about it, and it is why the index lane has walked past.
  const badge = li.querySelector(".badge");
  const state = clip === editing ? "editing" : clip.state;
  setText(
    badge,
    { ready: "Smart", error: "エラー", indexing: "解析中", editing: "編集中" }[state] ||
      "解析待ち"
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
    setText(cmBadge, blocks ? `CM ${blocks}` : "CM なし");
    cmBadge.className = `cmbadge ${blocks ? "found" : "empty"}`;
  }

  const running = clip.state === "indexing" || clip.cmState === "running";
  const pct = clip.state === "indexing" ? clip.progress : clip.cmProgress;
  li.querySelector(".pbar").hidden = !running;
  li.querySelector(".pbar span").style.width = `${Math.round(pct * 100)}%`;
  setText(
    li.querySelector(".ptext"),
    running
      ? `${clip.phase && clip.state === "indexing" ? clip.phase : "CM 検出"} ${Math.round(pct * 100)}%`
      : clip.state === "queued"
        ? "待機中"
        : clip.phase
  );
}

function paintTotals() {
  const known = clips.filter((c) => c.info);
  const total = known.reduce((n, c) => n + c.info.duration, 0);
  const pending = clips.length - known.length;
  el("clip-total").textContent =
    `クリップ合計数: ${clips.length}　合計時間: ${coarse(total)}` +
    (pending ? `（未解析 ${pending} 本を除く）` : "");
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
  el("stop-batch").textContent = paused && queued && !busy ? "解析を再開" : "解析を中止";
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
      ? `${picked.length} 個のクリップを選択中`
      : "クリップが選択されていません";
    return;
  }
  const c = picked[0];
  box.className = "props-body";
  if (!c.info) {
    box.textContent = c.state === "error" ? `${c.name}\n${c.error}` : `${c.name}\n解析待ちです`;
    return;
  }
  const i = c.info;
  const flags = [i.interlaced ? "インターレース (TFF)" : "プログレッシブ", i.pulldown ? "2:3プルダウン" : null]
    .filter(Boolean)
    .join(", ");
  const n = copyNo(c);
  box.textContent =
    `クリップ名:　${c.name}${n ? `（同じ録画の ${n} 本目）` : ""}\n` +
    `${c.path}\n` +
    `映像:　${i.codec}, ${i.width}x${i.height}, ${i.fps.toFixed(2)} fps, ${flags}\n` +
    `音声:　${i.has_audio ? "あり" : "なし"}\n` +
    `長さ:　${coarse(i.duration)} (${i.frames} フレーム)　無劣化点 ${i.points} 個` +
    (i.unusable_points ? `（うち ${i.unusable_points} 個は開始に使えません）` : "") +
    `\nシーン ${i.scenes} 箇所　索引 ${i.index_name}` +
    (c.cmPhase ? `\nCM:　${c.cmPhase}` : "");
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
  note("中止しています…");
  await invoke("stop_batch", { lane: null });
  note("解析を止めました。残りは「解析を再開」で続けられます");
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

function bindSetting(id, key, kind = "value") {
  const input = el(id);
  const read = () => {
    settings[key] = kind === "checked" ? input.checked : input.value;
    renderOutset();
    renderOutScreen();
  };
  input.addEventListener(kind === "checked" ? "change" : "input", read);
  if (kind === "checked") input.checked = settings[key];
  else input.value = settings[key];
}
bindSetting("out-dir", "dir");
bindSetting("out-prefix", "prefix");
bindSetting("out-container", "container");
bindSetting("out-audio", "audio");
bindSetting("out-keyframes", "keyframes", "checked");

el("browse-dir").addEventListener("click", async (ev) => {
  ev.preventDefault();
  const picked = await dialog.open({ directory: true, multiple: false });
  if (!picked) return;
  settings.dir = picked;
  el("out-dir").value = picked;
  renderOutset();
  renderOutScreen();
});

function renderOutset() {
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
    box.textContent = "解析の済んだクリップがありません";
    return;
  }
  const i = clip.info;
  const keeps = keepsOf(clip);
  const kept = keeps.reduce((n, k) => n + (k.b - k.a), 0);
  const audio = { smart: "スマートレンダリング", copy: "そのままコピー", reencode: "再エンコード" }[settings.audio];
  box.textContent =
    `映像:　${i.codec}, ${i.width}x${i.height}, ${i.fps.toFixed(2)} fps, ` +
    `${i.interlaced ? "インターレース (トップフィールド優先)" : "プログレッシブ"}\n` +
    `音声:　${i.has_audio ? audio : "なし"}\n` +
    `区間:　${keeps.length} 区間 / 出力 ${fmt(kept)}（元 ${fmt(i.duration)}、` +
    `カット ${clip.edit ? clip.edit.cuts.length : 0} 箇所）\n` +
    `出力先:　${outputPath(clip)}` +
    (settings.keyframes && clip.edit && clip.edit.keyframes.length
      ? `\n　　　　${outputPath(clip).replace(/\.[^./\\]*$/, "")}.keyframe`
      : "");
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
/// segments, and which of them is up.
let onShow = null;
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
  el("out-shots-note").textContent = "調べています…";
  stageShot(null, `${clipLabel(clip)} — 調べています…`);
  try {
    const r = await reencodeOf(clip);
    if (token !== shotsToken) return;
    onShow = { clip, r, at: -1 };
    const redone = r.segs.reduce((n, g) => n + g.frames, 0);
    if (!r.segs.length) {
      // Cuts that all landed on access points, or no cuts at all. Worth
      // saying rather than leaving it blank: it is the best outcome this
      // program has.
      el("out-shots-note").className = "grow lossless";
      el("out-shots-note").textContent = `${clipLabel(clip)} — なし。全編を無劣化コピーします`;
      // The clip's own poster rather than a black rectangle. It does not
      // contradict what this screen is for: the line under it says there is
      // nothing to re-encode, so the picture is standing for the clip about
      // to be written and not for a frame being made again.
      stageShot(null, "再エンコードなし — 全編を無劣化コピー", clip.info.poster);
      return;
    }
    el("out-shots-note").className = "grow dim";
    el("out-shots-note").textContent =
      `${clipLabel(clip)} — ${r.segs.length} 箇所 / ${redone} フレーム（ほかはバイト単位でコピー）`;
    stageShot(0);
  } catch (e) {
    if (token !== shotsToken) return;
    shownReencode = null;
    el("out-shots-note").className = "grow dim";
    el("out-shots-note").textContent = `調べられません: ${e}`;
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
  el("out-ovl-kind").textContent = `再エンコード ${i + 1} / ${r.segs.length}`;
  el("out-ovl-note").textContent = `${g.frames} フレーム`;
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
  el("out-dir-shown").value = settings.dir || "（入力ファイルと同じ場所）";
  const list = ready();
  el("out-idle").hidden = list.length > 0;
  // Idle, the screen speaks for whichever clip is about to be written first;
  // running, `runExport` points it at the one under the head.
  if (!exporting) showReencode(list[0] || null);
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
  el("out-elapsed").textContent = `経過 ${clock(spent)}`;
  el("out-left").textContent =
    overall > 0.01 ? `残り ${clock((spent / overall) * (1 - overall))}` : "残り --:--:--";
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
  el("out-state").textContent =
    "中止します（いま書き出しているクリップは最後まで書き終えます）";
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
  list.forEach((c) => (c.out = { state: "waiting", progress: 0, note: "待機中" }));
  el("abort-export").disabled = false;
  paintButtons();
  renderOutScreen();

  let done = 0;
  for (const clip of list) {
    if (abort) {
      clip.out = { state: "skipped", progress: 0, note: "中止" };
      continue;
    }
    const out = outputPath(clip);
    if (out === clip.path) {
      clip.out = { state: "error", progress: 0, note: "入力と同じ名前になります" };
      renderOutScreen();
      continue;
    }
    writing = clip;
    clip.out = { state: "running", progress: 0, note: "0%" };
    el("out-state").textContent = `"${nameOf(out)}" を出力中: 映像を無劣化出力しています…`;
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
          extra = ` / キーフレーム ${n} 個`;
        }
      }
      clip.out = { state: "done", progress: 1, note: `完了${extra}` };
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
  el("out-state").textContent =
    `${done} / ${list.length} 本を出力しました` +
    (failed ? `　失敗 ${failed} 本` : "") +
    (abort ? "　（中止されました）" : "") +
    `　経過 ${clock((Date.now() - began) / 1000)}`;
  paintOutProgress(1);
  paused = false;
  pump();
  renderOutScreen();
}

// --- keys on the list ---------------------------------------------------
//
// Only while the list is the screen on show. The editor has its own key
// handler, and Ctrl+D means the same thing on both screens for one clip and
// for many.

window.addEventListener("keydown", (ev) => {
  if (screen !== "input") return;
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
renderList();
show("input");
invoke("initial_paths")
  .then(async (paths) => {
    if (!paths || !paths.length) return;
    // Launched on files, from a file manager or the command line. They go
    // into the list like any others; a single one goes straight on into the
    // editor, which is what happened before there was a list. Several do
    // not -- being handed a batch is a reason to be shown the batch.
    jlog(`initial_paths -> ${paths.join(", ")}`);
    const taken = await addPaths(paths);
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
