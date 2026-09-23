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

import { fmt, clock, coarse, chLabel, cmNote, esc, size, blankKey, flatKey, noBrowserMenu, noNativeDrag, wireDrops }
  from "./shared.js";
import { t, applyStatic, preference, currentLang, setLang, onLangChange, tellBackend, confirmWithOs }
  from "./i18n.js";
import * as prefs from "./prefs.js";

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

const VIDEO_EXT = [
  "ts", "m2ts", "mts", "m2t", "mp4", "mkv", "mov", "m4v",
  // A program stream: what a DVD is written in, and what a `.mpg` from an
  // older recorder is. Read like any other file; written out as a transport
  // stream, for the reason `PS_LIKE` gives.
  "vob", "mpg", "mpeg", "m2p",
  // Matroska under another name, and the one shape VP9 and AV1 arrive in.
  // Written back as itself; which lists may be, and which may go into each
  // of the other containers, is the engine's answer. See `lockContainer`.
  "webm",
];
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

/// The name a row goes by: the one somebody typed over it, or the one it
/// arrived with -- a filename, or what a disc's index called the programme.
///
/// A rename is held beside the arrival name rather than over it, which is what
/// lets an emptied field mean "back to what it was called" rather than "called
/// nothing". See `startRename`.
const clipName = (clip) => clip.renamed || clip.name;

/// What to call a row anywhere it is named to the user. Two duplicates share
/// a filename, so the name alone stops identifying which one is meant the
/// moment there are two of them.
function clipLabel(clip) {
  const n = copyNo(clip);
  const name = clipName(clip);
  return n ? t("list.copyLabel", { name, n }) : name;
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

/// `found` is a path, and for a recording on a disc the five things a path
/// cannot say -- what the programme is called, what to name a cut of it,
/// where that cut can be written, where the disc set its chapter points, and
/// which of its tracks were switched off in the chooser. None of those is
/// derivable from `…/Anime.iso/BDMV/STREAM/00014.m2ts`.
///
/// What a clip says about the crossing that follows it, before anybody has
/// said anything.
///
/// Held as the engine's own names rather than as numbers -- `dissolve`,
/// `wipe-left`, `sine`, `in-out` -- so that a project file written by one
/// version and read by another either knows a name or does not. A name this
/// build has never heard of is no transition at all, which is the right
/// answer for a file from a later version and costs nothing for one from an
/// earlier.
const NO_CROSSING = {
  kind: "none",
  seconds: 1,
  curve: "none",
  mode: "in",
  image: "",
  /// How long the sound takes to leave at the end of the clip before this
  /// join, and to come back at the start of the clip after it, in seconds.
  /// Nought for both: what a fade fades is the programme.
  fadeOut: 0,
  fadeIn: 0,
};

/// Whether a join carries a setting worth writing down.
///
/// Two things happen at a join and either of them on its own counts: what
/// the pictures do, and what the sound does. A fade under no crossing at all
/// is an ordinary thing to ask for -- most joins between two programmes want
/// exactly that -- and asking whether the *kind* was set would have thrown
/// it away on the way into the project file.
const crossingSet = (after) =>
  !!after && (after.kind !== "none" || after.fadeOut > 0 || after.fadeIn > 0);

/// A path for a file, one of those for a recording on a disc, and a saved row
/// out of a project, which is the same shape written down.
function makeClip(found) {
  const { path, name, renamed, stem, home, chapters, dropPids, made, description,
          channel, channelNumber, programme, after, edited } =
    typeof found === "string" ? { path: found } : found;
  return {
    // A row's own identity, which its path is not: the same recording can be
    // in the list more than once, cut two different ways. Everything about a
    // *row* is addressed by this; the path addresses the file, and the two
    // stopped being the same thing when clips became duplicable.
    id: nextId++,
    path,
    name: name || nameOf(path),
    /// What somebody renamed this row to, or null for the name it arrived
    /// with.
    ///
    /// The row's answer and not the file's: two rows on one recording can be
    /// called two things, which is half of what duplicating one is for. It is
    /// what the row shows, what a cut of it is written as, and -- unless the
    /// output screen has been told otherwise -- what a disc's index calls the
    /// programme.
    renamed: renamed || null,
    /// What a cut of it is called and where it goes, when the recording's own
    /// path cannot answer either. Null for an ordinary file.
    stem: stem || null,
    home: home || null,
    /// The chapter points the disc's index carried, on the recording's own
    /// clock. Held here rather than turned into marks on the spot: only the
    /// editor knows where the container's clock begins, and it is the one
    /// that owns marks. Empty for an ordinary file.
    chapters: chapters || [],
    /// Streams switched off when the disc was read, by PID.
    ///
    /// A PID rather than a stream index because this answer was given before
    /// anything was opened -- the disc's index names a track by the PID it
    /// sits on, and a stream index is something libavformat makes up once it
    /// has read the recording. Resolved on the far side: by the editor when
    /// this row is opened in it, and by the backend when it is written out.
    ///
    /// It is the answer only until the editor gives one. The track menu
    /// there writes `edit.dropStreams`, and from that moment the edit is what
    /// speaks for this row -- otherwise a track switched back *on* in the
    /// editor would be switched off again on the way out by an answer given
    /// before anybody had seen the recording.
    dropPids: dropPids || [],
    /// What a disc's index will say about the recording besides its name:
    /// when it was made, as `2026-08-17 01:00:00`, what it was about, and the
    /// channel it came off with the three digits a viewer knows that channel
    /// by. What the disc this came off said, until somebody types over it on
    /// the output screen.
    ///
    /// Null is nobody having said, and falls through to what the recording
    /// says about itself -- the answer a file arrives with, asked for when it
    /// is needed. An empty string is an answer: leave the field blank on the
    /// disc. See `madeOf`.
    made: made ?? null,
    description: description ?? null,
    channel: channel ?? null,
    channelNumber: channelNumber ?? null,
    /// What to call this recording in a disc's index, when the name it
    /// arrived with is not the one wanted. Null until somebody types one,
    /// which is the difference between "no answer yet" and "called nothing".
    programme: programme === undefined ? null : programme,
    /// What happens where this clip gives way to the next one, when the list
    /// is being written as a single file.
    ///
    /// **On the clip that gives way**, which is where the reference tool
    /// puts it and the only place it reads in the order the file is written.
    /// Null is what every row starts as and what the list is full of:
    /// nothing between the clips, which is what a join has always been.
    after: after ? { ...NO_CROSSING, ...after } : null,
    /// What the recording itself says about its programme: the name, the
    /// channel and when it went out. Null until asked, `{}` where the
    /// recording says nothing -- which is not the same question.
    said: null,
    info: null,
    /// What the container said about the recording, had for the cost of an
    /// open the moment the row was made. Everything the row's own line shows
    /// is in here, so the row is legible long before `info` exists; what is
    /// not in here is what only the walk can answer -- the lossless points,
    /// the pulldown, and a program stream's real length. Null until asked,
    /// and stays null for a file that would not open at all.
    ///
    /// Never replaces `info`. Once the walk has been over the recording its
    /// answer is the better one on every field the two share.
    outline: null,
    state: "queued", // queued | indexing | ready | error
    /// Where the clip's key pictures are: null before the walk has finished,
    /// then queued | running | done | error. Its own state because it is its
    /// own pass -- the row is Smart and can be cut and detected without it,
    /// and it runs behind every row's walk rather than behind each row's.
    pics: null,
    scenes: null,
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
    /// The flat-picture detection and the quiet-sound one: how many stretches
    /// each found, and how far each has got. Two of everything because they
    /// are two lanes -- the pictures are read in a minute and the sound in
    /// seconds, and a row can be waiting for one while the other is done.
    ///
    /// Neither is carried across to the editor the way `cmPending` is. That
    /// window reads the same cache these lanes write, and it reads it on the
    /// way in -- so what the list found is on the timeline before anybody
    /// asks, without the two windows having to agree about whose answer is
    /// newer.
    blankState: "none", // none | queued | running | done | error
    blankPhase: "",
    blankProgress: 0,
    blankFound: null,
    /// "cache" where the count came out of an earlier session or the cut
    /// editor rather than from a pass this window made, which is the one
    /// thing the sentence says that the count does not -- and is why the
    /// sentence can be written again in another language. See `relocalise`.
    blankSource: null,
    quietState: "none",
    quietPhase: "",
    quietProgress: 0,
    quietFound: null,
    quietSource: null,
    edit: null,
    /// Whether the cut editor has ever been used on this row.
    ///
    /// **Not "has it anything on its timeline".** A list read for commercials
    /// comes back with marks on every row and blocks on most of them, and a
    /// detection is not something anybody decided: the question this answers
    /// is which rows have been looked at and settled, which on an evening's
    /// twenty recordings is the only thing separating the work that is done
    /// from the work that is waiting.
    ///
    /// So it is set where an edit arrives that the editor did not open with
    /// -- see the `editor-state` handler, which measures against the state
    /// the window was given -- and it is not set by a detection landing,
    /// whichever window ran it. Written into the project, because a list
    /// reopened next week is exactly when the question is asked.
    edited: !!edited,
    /// The row's picture, out of the thumbnail track and taken against
    /// whatever the cuts leave. Null until the picture pass has been over the
    /// clip; `glance` stands for it until then.
    poster: null,
    /// The ranges `poster` was taken for, so that a state arriving from the
    /// editor twice a second does not ask for the same picture again.
    posterSig: null,
    /// A picture taken the cheap way when the row was added, so that the row
    /// is not blank for as long as the queue in front of it takes. Null until
    /// it has been asked for, "" once it has been asked for and there was
    /// none. Second in `posterOf`: `poster` comes out of the thumbnail track
    /// and knows where the cuts are, and this does not.
    glance: null,
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

const SCREENS = {
  input: "screen-input",
  outset: "screen-outset",
  out: "screen-out",
  batch: "screen-batch",
};
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
  showBatchMenu(false);
  closeJobMenu();
  closeRowMenu();
  if (name === "outset") renderOutset();
  // Coming to the screen is asking it what it has to say, so it goes back to
  // speaking for the list. A run's last frame is held for whoever watched the
  // run end, not kept over the top of the next question.
  if (name === "out") {
    heldAfterRun = false;
    renderOutScreen();
  }
  if (name === "batch") renderBatch();
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
/// ...and whether the row was already marked as edited then, for the same
/// reason. See `edited` on a clip.
let editedBefore = false;

/// Whether an editor window is being built right now.
///
/// Every `editor-closed` that lands while it is is about the window before
/// this one: the window being built cannot have closed yet. See the handler.
let opening = false;

async function edit(clip) {
  // A row that has left the list. Nothing here asks for one on purpose, but a
  // press can arrive after the row it was aimed at has gone -- the listeners
  // of a deleted row go with the row only when nothing is still holding the
  // event that reached it. Opening the editor on one would be a window about
  // a recording the list no longer has: it cannot be got back to from the
  // list, and what it says it is cancelling is an edit nothing is keeping.
  if (!clips.includes(clip)) {
    jlog(`edit ${clipName(clip)}: gone from the list`);
    return;
  }
  jlog(`edit ${clipName(clip)} (${clip.state})`);
  // A clip that could not be read has nothing to open. Anything else can be
  // opened whenever it is asked for -- the editor makes its own way through a
  // recording that has never been read, showing what it has got to as it
  // goes, so waiting for the list's turn buys nothing.
  if (clip.state === "error") {
    note(t("list.cannotRead", { clip: clipLabel(clip) }));
    return;
  }
  // And a recording that was there when the row was made and is not there
  // now. A name renamed in the file manager is the ordinary way into this,
  // and the list has no way of hearing about it: the row goes on holding the
  // name it was added with. Answered here rather than by opening the window
  // and letting the read fail, because a cut editor with nothing in it is a
  // window somebody has to work out how to get rid of.
  //
  // The row is marked with it. This is the same thing the walk says when it
  // cannot read a recording, it is about the file rather than about this
  // press, and a row that stays looking ready is a row somebody opens again.
  // A question that cannot be answered is not a missing file: the editor
  // opens and says for itself what it found.
  if (await invoke("clip_gone", { path: clip.path }).catch(() => false)) {
    clip.state = "error";
    clip.error = t("list.gone", { path: clip.path });
    clip.phase = "";
    paintRow(clip);
    paintButtons();
    note(t("list.goneNote", { clip: clipLabel(clip) }));
    return;
  }
  // Asked again, because that question was answered on the other side of the
  // wire and the list went on taking presses while it was over there: the row
  // can have been deleted in the time the answer took.
  if (!clips.includes(clip)) return;
  editing = clip;
  before = clip.edit ? JSON.parse(JSON.stringify(clip.edit)) : null;
  editedBefore = clip.edited;
  // A lane in flight on this very clip is left alone. It used to be stopped
  // here -- the editor is about to make the same pass, and reading one file
  // twice at once is the thing most worth avoiding -- but that traded a real
  // loss for a notional saving. What it threw away was however far the pass
  // had got: a row four minutes into a five-minute walk fell back to 解析待ち,
  // and the editor then started the same walk from zero. What it saved was
  // less than it looked, because the two reads are not two trips to the disc.
  // The editor's read follows the lane's through the page cache the lane is
  // filling, and the index writer has expected the pair since it was written
  // -- it renames a temporary named apart from every other one into place for
  // exactly this case, the list indexing a row while the editor opens that
  // same file.
  //
  // The pictures lane is the one this costs anything real: its pass and the
  // editor's decode the same key pictures at the same time, on a machine the
  // editor has already cut the lanes back to a share of. It is left running
  // all the same. What it has decoded stays decoded, the row keeps its own
  // picture and its scene marks rather than losing both to a window being
  // opened, and the track it writes is what the next open of this clip reads
  // instead of decoding again.
  //
  // What is still passed over is *starting* a pass on this clip; see
  // `nextFor`. Beginning work the editor is already doing duplicates it with
  // nothing part-finished to save.
  try {
    opening = true;
    await invoke("open_editor", { title: t("editor.windowTitle", { clip: clipLabel(clip) }) });
    // Lost if the window is still starting up, which is what `editor-ready`
    // is for; sent anyway for the case where it is already open on another
    // clip and there will be no `editor-ready` at all.
    tellEditor();
  } catch (e) {
    note(t("list.cannotOpenEditor", { e }));
    editing = null;
    before = null;
  } finally {
    opening = false;
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
    // What the row is called: what somebody renamed it to, and failing that
    // what it arrived as -- which for a recording on a disc is the programme
    // rather than `00001.m2ts`.
    name: clipName(editing),
    // Where a keyframe list beside the recording would be, without the
    // extension. Beside the disc for a recording on one -- inside an image
    // there is nothing to be beside -- and under the same name a cut of it
    // is written under, which is where the export writes one.
    side: sidecarBase(editing),
    // The disc's chapter points, which only the editor can turn into marks --
    // it is the one that knows where the recording's clock begins. Sent
    // whether or not this row has been in there before; the editor puts them
    // down on a first visit only, the same as the marks beside the file.
    chapters: editing.chapters,
    // Tracks switched off in the chooser when the disc was read, by PID. The
    // editor turns them into its own answer on a first visit and owns it from
    // then on -- see `makeClip`.
    dropPids: editing.dropPids,
    saved: editing.edit,
    // Blocks a batch detection found that the timeline has not been shown
    // yet. Only the editor can turn them into marks -- it is the one that
    // knows where the material begins.
    // The whole finding and not only its blocks: how the detection read the
    // recording is what lets that window say what was found in the language
    // that is up, and write it beside the recording if it is asked to.
    cm:
      editing.cmPending && editing.cm
        ? {
            blocks: editing.cm.blocks,
            logo_found: editing.cm.logo_found,
            resets: editing.cm.resets,
            note: editing.cmPhase,
          }
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
    // Has somebody done something in there? The editor answers it, having
    // the one thing needed to: what the timeline held when the recording
    // finished arriving. The marks beside the file, a disc's chapters and
    // every detection are not edits, whichever window ran them.
    if (state.touched) clip.edited = true;
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
    // The row's picture is about the cuts as much as the line under the name
    // is. It repaints the row itself when it has one, and says nothing when
    // the cuts have not moved.
    refreshPoster(clip);
    // The other way a project changes: a cut made in the other window. It
    // repaints one row rather than the list, so it says so itself.
    touch();
    if (screen === "outset") renderOutset();
    if (screen === "out") renderOutScreen();
  });

  listen("editor-cancel", (ev) => {
    const clip = byId(ev.payload) || editing;
    if (clip) clip.edit = before;
    // Along with the mark that says the row has been edited: what is being
    // put back is the row as the window found it, and a row cancelled out of
    // has not been edited unless it had been before.
    if (clip) clip.edited = editedBefore;
    paintList();
    // Put back the picture the cuts being put back are of.
    if (clip) refreshPoster(clip);
  });

  // The editor window has gone -- by OK, by キャンセル, or by its own cross.
  // Whichever it was, what it did is already here. What is left is the row
  // itself: it was passed over while the editor had it, and if it was never
  // read the lane can have it now -- cheaply, since the editor will have
  // written its seek index.
  //
  // **Not every one of these is about the window that is up.** Close the
  // editor and open another row straight away, and the news of the first
  // window going can be read after the second one has been asked for -- the
  // message and the double-click come to this window down different roads.
  // Acting on it then let go of the row that had just been opened, and the
  // new window sat there empty: it asks for its clip by name and the answer
  // to that is the row this holds, so a row let go of is a cut editor that
  // never loads anything and can only be closed. Reported as a window that
  // now and then came up blank and came up properly on the third try, which
  // is exactly what a race between two messages looks like from the outside.
  //
  // So the window is asked whether one is actually up, and a close that
  // arrives while another is being built is passed over on the spot -- the
  // window being built cannot be the one that has closed.
  listen("editor-closed", async () => {
    // An unanswerable question is taken as "no window", because the cost of
    // the two mistakes is not the same: a close passed over for good leaves
    // the list thinking it is still editing, with the row wearing 編集中 and
    // the lanes walking past it until the program is restarted.
    if (opening || (await invoke("editor_up").catch(() => false))) return;
    // What that window found while it had the row. Its commercial detection
    // comes back inside the edit (`editor-state`); the two flat ones are in
    // the cache, and this is where the row goes and reads them.
    const was = editing;
    editing = null;
    before = null;
    paintList();
    pump();
    if (was) restoreFlat(was);
  });

  // 継ぎ目の編集's own three. It asks for the joins when its page is up, hands
  // the settings back on OK, and says nothing at all on キャンセル -- which is
  // what makes it a cancel: nothing in the list is touched until `cross-done`
  // arrives.
  listen("cross-ready", () => tellCross());

  listen("cross-done", (ev) => {
    const said = (ev.payload && ev.payload.joins) || [];
    let changed = false;
    for (const answer of said) {
      const clip = byId(answer.id);
      if (!clip) continue;
      const was = JSON.stringify(clip.after || null);
      // Written back as the window left it, including `null` for a join it
      // cleared: a crossing nobody has said anything about is absent rather
      // than present and switched off, which is what `NO_CROSSING` is the
      // shape of and what `makeClip` starts every row at.
      clip.after = answer.after ? { ...NO_CROSSING, ...answer.after } : null;
      if (JSON.stringify(clip.after || null) !== was) changed = true;
    }
    if (!changed) return;
    renderCrossing();
    renderOutScreen();
    touch();
  });

  listen("cross-closed", () => {
    if (crossOpening) return;
    // The plan and the line beside the button both describe what the
    // transitions cost, and either may have been changed in there.
    renderCrossing();
    renderOutScreen();
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
  const found = resolved.flatMap((r) => r.files);
  const taken = [];
  const skipped = [];
  for (const f of found) {
    // A disc is a question rather than a file: which of the sixty-two things
    // on it did you mean, and which of their tracks. Asked one disc at a
    // time, in the order the discs turned up, and the list is left alone
    // until it has been answered.
    if (f.what === "disc") {
      for (const chosen of await chooseFromDisc(f)) {
        if (byPath(chosen.path)) continue;
        const clip = makeClip(chosen);
        clips.push(clip);
        taken.push(clip);
      }
      continue;
    }
    if (!VIDEO_EXT.includes(extOf(f.path))) {
      skipped.push(nameOf(f.path));
      continue;
    }
    if (byPath(f.path)) continue;
    const clip = makeClip(f);
    clips.push(clip);
    taken.push(clip);
  }
  // What has just arrived is what the next thing done to the list is about, so
  // it is what is selected -- and whatever was selected before is not. Three
  // recordings dropped onto a list of twenty are three rows to detect, move or
  // rename, and finding them again afterwards is work the drop already did.
  if (taken.length) {
    clips.forEach((c) => (c.selected = false));
    taken.forEach((c) => (c.selected = true));
    anchor = clips.indexOf(taken[0]);
  }
  renderList();
  if (taken.length) taken[0].row.scrollIntoView({ block: "nearest" });
  // One at a time rather than a hundred at once: each is a stat on whatever
  // the recordings are on, and the answer is wanted before anyone looks at
  // the row rather than this instant.
  (async () => {
    for (const clip of taken) {
      await restoreCm(clip);
      await restoreFlat(clip);
    }
  })();
  fillFirstLook();
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

// --- ディスクの読み込み --------------------------------------------------
//
// A file is one recording and needs no question asked about it. A disc is
// not: a pressed Blu-ray holds twelve episodes among fifty logos, warnings,
// menu loops and eight second transitions, and its own index calls all
// sixty-two of them `000NN`. Adding them all would make the list a thing to
// prune; adding the ones this program guessed at would be guessing.
//
// So the disc's index is laid out and the question is asked once, before
// anything is opened -- which is why every answer here is in terms the index
// can give: a length, a size, and a track named by the PID it sits on.

/// What the chooser is looking at, or null when it is not open.
let chooser = null;
/// The discs already waiting to be asked about. Two drops can be in flight at
/// once -- a folder being walked while a second is dropped on top of it -- and
/// there is one dialog, so they queue rather than the second one taking the
/// first one's window out from under it.
let asking = Promise.resolve();

/// Ask which of a disc's recordings to add, and which of their tracks.
///
/// Resolves to the rows that were ticked, ready for `makeClip`. Cancelling
/// resolves to none of them, which is the same as never having dropped the
/// disc.
function chooseFromDisc(disc) {
  const answered = asking.then(() => askAboutDisc(disc));
  // The queue only sequences; a failure in one dialog is not the next one's
  // business.
  asking = answered.catch(() => {});
  return answered;
}

function askAboutDisc(disc) {
  return new Promise((resolve) => {
    chooser = {
      disc,
      resolve,
      // Per clip, and beside the disc's own row rather than in it: whether it
      // is coming, which of its tracks are switched off, and whether its
      // tracks are on show.
      state: disc.clips.map((c) => ({ take: !!c.wanted, drop: [], open: false })),
      // A disc of recordings is a disc of things somebody chose to record, so
      // there is nothing to hide; a pressed disc -- Blu-ray or DVD -- is
      // mostly not the film, and opening on all sixty-two rows would bury the
      // twelve that were meant.
      showAll: disc.kind === "bdav",
    };
    // Said before anything is ticked rather than after: the index of such a
    // disc reads and every stream it names does not, so the list below is
    // complete and none of it can be opened.
    el("disc-protected-row").hidden = !disc.protected;
    el("disc-show-all").checked = chooser.showAll;
    el("disc-show-all-row").hidden = disc.kind === "bdav";
    el("disc-what").textContent = t("disc.what", {
      label: disc.label,
      kind: t(`disc.kind.${disc.kind}`),
      n: disc.clips.length,
    });
    renderChooser();
    el("disc").hidden = false;
    el("disc-ok").focus();
  });
}

/// Put the chooser away, answering what was ticked.
function closeChooser(take) {
  if (!chooser) return;
  const { disc, state, resolve } = chooser;
  chooser = null;
  el("disc").hidden = true;
  el("disc-list").innerHTML = "";
  if (!take) {
    resolve([]);
    return;
  }
  resolve(
    disc.clips
      .map((c, i) => ({ clip: c, st: state[i] }))
      .filter(({ st }) => st.take)
      .map(({ clip, st }) => ({
        path: clip.path,
        name: clip.label,
        stem: clip.stem,
        home: clip.home,
        chapters: clip.chapters,
        made: clip.made,
        ran: clip.ran,
        description: clip.description,
        channel: clip.channel,
        // Nought is the disc's index saying it does not know, which is not
        // the same as a row with no number in it -- so it arrives as nobody
        // having said, and the stream is still asked. See `channelNumberOf`.
        channelNumber: clip.channel_number || null,
        dropPids: st.drop.slice(),
      }))
  );
}

/// Which rows are on show. A short clip nobody has ticked is out of the way
/// until it is asked for; one that *is* ticked stays visible however short it
/// is, because a row that vanished when it was chosen would be a row that
/// could not be unchosen.
function shownRows() {
  const { disc, state, showAll } = chooser;
  return disc.clips
    .map((_, i) => i)
    .filter((i) => showAll || disc.clips[i].wanted || state[i].take);
}

function renderChooser() {
  if (!chooser) return;
  const { disc, state } = chooser;
  const list = el("disc-list");
  list.innerHTML = "";
  const shown = shownRows();
  for (const i of shown) {
    const clip = disc.clips[i];
    const st = state[i];
    const li = document.createElement("li");
    const row = document.createElement("div");
    row.className = "disc-row";

    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = st.take;
    box.id = `disc-take-${i}`;
    box.addEventListener("change", () => {
      st.take = box.checked;
      paintChooserCount();
    });
    const label = document.createElement("label");
    label.className = "name";
    label.htmlFor = box.id;
    label.textContent = clip.label;
    // The path is the one thing the row cannot show and somebody will want:
    // which of the disc's streams this actually is.
    label.title = clip.path;
    const len = document.createElement("span");
    len.className = "num";
    len.textContent = clock(clip.duration);
    const bytes = document.createElement("span");
    bytes.className = "num";
    bytes.textContent = size(clip.bytes);
    row.append(box, label, len, bytes);

    if (clip.tracks.length) {
      const more = document.createElement("button");
      more.type = "button";
      more.className = "more";
      more.textContent = `${st.open ? "▾" : "▸"} ${t("disc.tracks", { n: clip.tracks.length })}`;
      more.addEventListener("click", () => {
        st.open = !st.open;
        renderChooser();
      });
      row.append(more);
    } else {
      const none = document.createElement("span");
      none.className = "dim";
      none.textContent = t("disc.noTracks");
      row.append(none);
    }
    li.append(row);
    if (st.open && clip.tracks.length) li.append(trackPanel(i));
    list.append(li);
  }

  const hidden = disc.clips.length - shown.length;
  if (hidden > 0) {
    const li = document.createElement("li");
    const row = document.createElement("div");
    row.className = "disc-row dim";
    row.textContent = t("disc.hidden", { n: hidden });
    li.append(row);
    list.append(li);
  }
  paintChooserCount();
}

/// The tracks under one row.
function trackPanel(at) {
  const { disc, state } = chooser;
  const clip = disc.clips[at];
  const st = state[at];
  const ul = document.createElement("ul");
  ul.className = "disc-tracks";

  clip.tracks.forEach((track, n) => {
    const li = document.createElement("li");
    const box = document.createElement("input");
    box.type = "checkbox";
    box.id = `disc-track-${at}-${n}`;
    // Three cases, and only one of them is a choice. The video is what a cut
    // is *of*; the graphics a Blu-ray's subtitles and menus are made of
    // cannot go on a cut timeline at all, and are listed to say they are
    // being left behind rather than to offer anything about them.
    const fixed = track.kind === "video" || !track.carried;
    box.checked = track.carried && !st.drop.includes(track.pid);
    box.disabled = fixed;
    if (!fixed) {
      box.addEventListener("change", () => {
        st.drop = box.checked
          ? st.drop.filter((pid) => pid !== track.pid)
          : st.drop.concat([track.pid]);
      });
    }
    const label = document.createElement("label");
    label.htmlFor = box.id;
    const bits = [t(`disc.${track.kind}`), track.detail];
    if (track.language) bits.push(track.language);
    bits.push(t("disc.pid", { pid: track.pid.toString(16).padStart(4, "0") }));
    if (!track.carried) bits.push(t("disc.gone"));
    else if (track.kind === "video") bits.push(t("disc.needed"));
    label.textContent = bits.join(t("sep"));
    if (fixed) label.className = "gone";
    li.append(box, label);
    ul.append(li);
  });

  // Every episode on a disc carries the same tracks, and answering the same
  // question twelve times is not answering it once. Offered only where there
  // is more than one row it would apply to and something on them to switch
  // off, so that a disc of one recording -- or one whose only tracks are the
  // video and the graphics beside it -- does not grow a button that does
  // nothing.
  const like = sameTracks(at);
  const choosable = clip.tracks.some((k) => k.carried && k.kind !== "video");
  if (like.length > 1 && choosable) {
    const apply = document.createElement("button");
    apply.type = "button";
    apply.className = "mini apply";
    apply.textContent = t("disc.apply");
    apply.addEventListener("click", () => {
      for (const other of like) state[other].drop = st.drop.slice();
      renderChooser();
      el("disc-count").textContent = t("disc.applied", { n: like.length });
    });
    ul.append(apply);
  }
  return ul;
}

/// Which rows carry the same tracks as this one, this one included.
///
/// The same by what the disc says about them -- the kind, the PID, the codec
/// and the language -- because that is the whole of what a track choice was
/// made against.
function sameTracks(at) {
  const { disc } = chooser;
  const shape = (c) =>
    c.tracks.map((k) => `${k.kind}:${k.pid}:${k.detail}:${k.language || ""}`).join("|");
  const want = shape(disc.clips[at]);
  return disc.clips.map((_, i) => i).filter((i) => shape(disc.clips[i]) === want);
}

function paintChooserCount() {
  if (!chooser) return;
  const n = chooser.state.filter((st) => st.take).length;
  el("disc-count").textContent = n ? t("disc.count", { n }) : t("disc.countNone");
  // Nothing ticked and 読み込む would mean the same as キャンセル, which is a
  // button that says one thing and does another.
  el("disc-ok").disabled = n === 0;
}

el("disc-all").addEventListener("click", () => {
  if (!chooser) return;
  // What is on show, and not the whole disc: ticking fifty rows nobody can
  // see is not what a button beside them says it does.
  for (const i of shownRows()) chooser.state[i].take = true;
  renderChooser();
});
el("disc-none").addEventListener("click", () => {
  if (!chooser) return;
  for (const st of chooser.state) st.take = false;
  renderChooser();
});
el("disc-show-all").addEventListener("change", () => {
  if (!chooser) return;
  chooser.showAll = el("disc-show-all").checked;
  renderChooser();
});
el("disc-ok").addEventListener("click", () => closeChooser(true));
el("disc-cancel").addEventListener("click", () => closeChooser(false));
window.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape" && chooser) closeChooser(false);
});

el("add-files").addEventListener("click", async () => {
  const picked = await dialog.open({
    multiple: true,
    filters: [
      { name: t("dialog.video"), extensions: VIDEO_EXT },
      // A disc image is one line in the picker and several in the list: what
      // is added is the recordings on it. A folder holding a disc is added
      // by dropping it, the same as a folder of recordings.
      { name: t("dialog.disc"), extensions: ["iso"] },
    ],
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
// Three lanes, each working down the list in order, one pass at a time, all
// three running together:
//
//   walk  -- reads every packet and writes the seek index. This is what makes
//            a row real: its length, its lossless points, its Smart badge.
//   pics  -- decodes the key pictures of a clip the walk has been over: the
//            row's picture, the scenes, and the scrub track the editor would
//            otherwise build when it opened.
//   cm    -- looks for commercial breaks.
//
// Three because they are three different loads and the machine has room for
// all of them at once. The walk is the disc: one core reading a gigabyte a
// second and touching no decoder. The pictures are the cores: every key
// picture through libavcodec at around four seconds a gigabyte. A detection
// is one core and a great deal of waiting -- libavcodec threads none of what
// it reads.
//
// Running the walk and the pictures side by side rather than one after the
// other is what makes the list quick. Serially they added up: the walk read
// the whole recording, and then the picture pass read the whole recording
// again, and neither was doing what the other was waiting on. Side by side
// the walk runs ahead through the list while the pictures follow it clip by
// clip -- so every row is real at disc speed, and the decoding costs almost
// nothing on the clock because it happens during reads it is not waiting for.
//
// The pictures follow the walk *closely* on purpose. They read the same
// recording the walk has just been through, so the second read is served out
// of what the first one left in the page cache rather than off the disc
// again. Sweeping all the walks first and all the pictures afterwards lost
// that -- measured at 18% off the picture passes -- because by the time a
// clip's pictures came up, three other recordings had been through the cache.
//
// The lanes run whether or not the cut editor is open. What gives way to the
// editor is not the work but its share of the machine -- `background_threads`
// on the Rust side hands the passes part of it while that window is up.

/// Which lanes have a pass in flight, so none is started twice.
const lanes = { walk: false, pics: false, cm: false, blank: false, quiet: false };
const running = () => lanes.walk || lanes.pics || lanes.cm || lanes.blank || lanes.quiet;

/// Raised by 解析を中止 and while an export is running. Not the same as an
/// empty queue: the work is still queued, it is just not being taken.
let paused = false;

/// The last thing worth saying that was not a lane saying it -- an error, or
/// what 中止 did. Shown when no lane has anything to report.
let sticky = "";

function note(text) {
  sticky = text;
  paintQueueNote();
}

/// One line, three lanes. Composed from what is running rather than written
/// by whichever pass spoke last, because all three want this line and none
/// may have it to itself.
///
/// And then the same sentence on the three screens that have no lanes to
/// share a line with. The lanes stay here: what they are doing is about the
/// list, and the list is this screen. What `sticky` holds is not -- a run is
/// started and watched on the output screen and says four or five things
/// while it goes, and a run that will not start sends somebody to the
/// settings screen to fix what stopped it. Both were being written onto a
/// line neither screen shows, and the batch tool, which is this same page
/// with the input screen never drawn, showed none of it at all. See `note`
/// and `sayHere`.
function paintQueueNote() {
  const bits = [];
  const ix = clips.find((c) => c.state === "indexing");
  const pic = clips.find((c) => c.pics === "running");
  const cm = clips.find((c) => c.cmState === "running");
  const blank = clips.find((c) => c.blankState === "running");
  const quiet = clips.find((c) => c.quietState === "running");
  if (ix) bits.push(t("queue.indexing", { clip: clipLabel(ix) }));
  if (pic) bits.push(t("queue.picturing", { clip: clipLabel(pic) }));
  if (cm) bits.push(t("queue.detecting", { clip: clipLabel(cm) }));
  if (blank) bits.push(t("queue.blank", { clip: clipLabel(blank) }));
  if (quiet) bits.push(t("queue.quiet", { clip: clipLabel(quiet) }));
  el("queue-note").textContent = bits.length ? bits.join(t("sep")) : sticky;
  el("out-note").textContent = sticky;
  el("row-out-note").hidden = !sticky;
  el("outset-note").textContent = sticky;
  el("batch-note").textContent = sticky;
}

/// The next clip for a lane, or nothing. Each lane works down the list in
/// order and takes only its own kind of work.
///
/// A clip is owed its pictures the moment its walk is done, so the two index
/// lanes end up one clip apart: the walk runs ahead through the list, the
/// pictures follow it. That is both the fast order and the cheap one -- the
/// row is real as soon as the walk passes it, and the pictures read a
/// recording the walk has just pulled through the page cache.
///
/// The clip in the editor is passed over by both index lanes: the editor is
/// making those passes itself. The clip being cut is not passed over by the
/// detection lane -- it reads a different part of the file, and detecting the
/// commercials of the clip you are cutting while you cut it is the point of
/// Ctrl+D.
///
/// What the detection lane does pass over is a row the walk has not been
/// over. A detection reads the seek index, so it cannot start before there
/// is one -- but the walk is on its way down the list and will reach every
/// row, so the queued detection waits there rather than being refused. That
/// is what makes 全選択 → Ctrl+D on a list still being read mean "detect
/// them all, each as its own reading finishes". See `detectSelected`, and
/// the `pumpLane("cm")` at the end of a walk.
function nextFor(lane) {
  if (lane === "walk") return clips.find((c) => c.state === "queued" && c !== editing);
  if (lane === "pics") return clips.find((c) => c.pics === "queued" && c !== editing);
  if (lane === "cm") return clips.find((c) => c.cmState === "queued" && c.state === "ready");
  // The two flat detections wait for the walk as the commercial one does, and
  // for the same reason: all of them are reserved on a list that is still
  // being read, and a row the walk has not reached yet is a row they would be
  // reading at the same time as the walk.
  if (lane === "blank") return clips.find((c) => c.blankState === "queued" && c.state === "ready");
  return clips.find((c) => c.quietState === "queued" && c.state === "ready");
}

function pump() {
  pumpLane("walk");
  pumpLane("pics");
  pumpLane("cm");
  pumpLane("blank");
  pumpLane("quiet");
}

const RUN = { walk: runIndex, pics: runPictures, cm: runCm, blank: runBlank, quiet: runQuiet };

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
      await RUN[lane](next);
    }
  } finally {
    lanes[lane] = false;
    paintButtons();
    paintQueueNote();
  }
}

/// The row's resting note: how its seek index was come by.
///
/// Written again after the picture pass as well as after the walk, because
/// what a pass leaves on the row while it runs is about the pass, and what
/// the row says when nothing is running should be about the clip.
const indexNote = (clip) =>
  clip.info.cached
    ? t("phase.indexReused")
    : t("phase.indexBuilt", { s: clip.info.seconds.toFixed(0) });

async function runIndex(clip) {
  clip.state = "indexing";
  clip.phase = t("phase.reading");
  clip.progress = 0;
  paintRow(clip);
  paintQueueNote();
  // Sent because this may turn out to be the pass that makes the pictures
  // too, in which case the poster is taken against what the cuts leave. See
  // `one_read` on the Rust side.
  //
  // Empty where nothing has been cut, which is nearly always: the ranges
  // would have to be worked out from the container's length and from a first
  // access point nothing has looked for yet, and the pass being asked is
  // about to know both exactly. Empty means "the whole of it, as you find
  // it", and the Rust side fills it in.
  const keeps = clip.edit && clip.edit.cuts.length ? rangesOf(clip) : [];
  const was = cutsSig(clip);
  try {
    clip.info = await invoke("index_clip", { path: clip.path, keeps });
    clip.state = "ready";
    clip.progress = 1;
    clip.phase = indexNote(clip);
    if (clip.info.pictures) {
      // A recording it would cost twice to read twice: the pictures came
      // back with the index, out of the one read, and the other lane has
      // nothing left to do for this row.
      takePictures(clip, clip.info.pictures, was);
    } else {
      // The ordinary case. The pictures are the other lane's work, and that
      // lane has usually run dry waiting for this -- the walk is a quarter of
      // what the pictures cost, so it gets ahead -- so it has to be started
      // again.
      clip.pics = "queued";
      pumpLane("pics");
    }
    // A detection asked for while this row was still unread. The lane looked
    // past it every time it came round, and this is the moment it can have
    // it -- so it is started again here, exactly as the pictures lane is.
    if (clip.cmState === "queued") pumpLane("cm");
    if (clip.blankState === "queued") pumpLane("blank");
    if (clip.quietState === "queued") pumpLane("quiet");
  } catch (e) {
    if (String(e).includes("cancelled")) {
      // Put back, not failed: 中止 means "not now", and the pass left
      // nothing behind to be inconsistent about.
      clip.state = "queued";
      clip.phase = t("phase.stopped");
    } else {
      clip.state = "error";
      clip.error = String(e);
      // A detection reserved on a recording that cannot be read is not work
      // waiting to happen: the walk it was waiting for is never coming, and
      // a row left saying 解析後に CM 検出 would wait for it for good.
      if (clip.cmState === "queued") {
        clip.cmState = "none";
        clip.cmPhase = "";
      }
      if (clip.blankState === "queued") {
        clip.blankState = "none";
        clip.blankPhase = "";
      }
      if (clip.quietState === "queued") {
        clip.quietState = "none";
        clip.quietPhase = "";
      }
    }
  }
  paintRow(clip);
  paintTotals();
  paintButtons();
  paintQueueNote();
  if (clip.selected) paintProps();
}

/// What a clip's cuts are, for telling whether they moved while a pass that
/// depended on them was in flight.
///
/// The cuts and not the ranges they work out to. The ranges are the cuts
/// measured against the recording's length and first access point, and the
/// walk changes both of those -- so a range string taken before a walk and
/// one taken after it differ for a clip nobody has touched.
const cutsSig = (clip) => JSON.stringify(clip.edit ? clip.edit.cuts : []);

/// Put a finished picture pass on the row, whichever pass produced it: the
/// pictures lane, or the walk itself where the recording was read only once.
///
/// `was` is what the cuts were when the pictures were asked for. They can
/// have moved on since -- the editor is open on this clip and somebody is
/// working in it -- and then this picture is of a clip that no longer exists.
/// Same test [`refreshPoster`] makes, for the same reason.
function takePictures(clip, got, was) {
  clip.pics = "done";
  clip.progress = 1;
  clip.scenes = got.scenes;
  clip.phase = indexNote(clip);
  if (got.poster && cutsSig(clip) === was) {
    clip.poster = got.poster;
    // The ranges as they are *now*, which is what `refreshPoster` compares
    // against -- and which the walk has just made exact.
    clip.posterSig = JSON.stringify(rangesOf(clip));
  }
}

/// Decode the clip's key pictures: its own picture, and its scenes.
///
/// Nothing the row *says* waits on this -- the walk in front of it settled
/// all of that -- so it is the last thing the lane does and the first thing
/// it gives up when 中止 is pressed. What it buys is the row's picture taken
/// against the cuts rather than the cheap one from `glance`, the scene count,
/// and a scrub track the editor would otherwise build when it opened.
///
/// The ranges are sent because the picture is taken from what survives: a
/// clip out of a project file arrives already cut, and a tenth of the way
/// into a broadcast recording is as often as not inside a commercial break.
/// So the answer is a poster that is already cut-aware, and `posterSig` is
/// set to what it was taken for -- exactly as [`refreshPoster`] would.
///
/// Not run at all for a recording the walk already made the pictures of; see
/// `one_read` on the Rust side.
async function runPictures(clip) {
  clip.pics = "running";
  clip.phase = t("phase.pictures");
  clip.progress = 0;
  paintRow(clip);
  paintQueueNote();
  const keeps = rangesOf(clip);
  const was = cutsSig(clip);
  try {
    takePictures(clip, await invoke("clip_pictures", { path: clip.path, keeps }), was);
  } catch (e) {
    if (String(e).includes("cancelled")) {
      clip.pics = "queued";
      clip.phase = t("phase.stopped");
    } else {
      // Not the row's state: the recording has been read and can be cut, and
      // a row that could not be decorated is not a row that failed. Said in
      // the line under the name and nowhere louder.
      clip.pics = "error";
      clip.phase = t("phase.noPictures");
      jlog(`clip_pictures: ${e}`);
    }
  }
  paintRow(clip);
  paintButtons();
  paintQueueNote();
  if (clip.selected) paintProps();
}

/// What has already been found about this recording's pictures and its sound,
/// out of the cache both windows write.
///
/// The badge answers "does this row still owe me an answer", so it has to
/// know about answers this window did not make: one from a session last night,
/// and one from the cut editor, which runs the same two passes on its own
/// buttons and writes the same files. So this is asked on the way in, as
/// `restoreCm` is, and again when the editor hands a row back.
///
/// Costs a stat and a page or two, and says nothing in the ordinary case
/// where nothing has been detected. A half that was found with a longer
/// minimum than the one in force now does not answer and is not counted --
/// the row would be claiming an answer to a question nobody asked. See
/// `flat_answers` on the Rust side.
async function restoreFlat(clip) {
  if (!invoke) return;
  const ask = flatAsk();
  // Before the walk there is no frame rate to convert with, and 30 is what
  // the rest of this window falls back to. It only matters where somebody has
  // typed the silence minimum in pictures, which is not the default.
  const fps = clip.info && clip.info.fps > 0 ? clip.info.fps : 30;
  let got;
  try {
    got = await invoke("flat_cached", {
      path: clip.path,
      minSeconds: ask.minSeconds,
      minPictures: ask.minPictures,
      black: ask.black,
      white: ask.white,
      levels: ask.levels,
      thresholdDb: ask.thresholdDb,
      quietSeconds: ask.quietInPictures ? ask.quietRun / fps : ask.quietRun,
    });
  } catch {
    // Unreachable recordings are the index pass's news to break.
    return;
  }
  if (!got) return;
  let said = false;
  for (const [which, runs] of [
    ["blank", got.blank],
    ["quiet", got.quiet],
  ]) {
    // A pass asked for or running in this window is the newer answer, and a
    // booked one must not be un-booked by what the cache used to hold.
    if (!runs || clip[`${which}State`] === "queued" || clip[`${which}State`] === "running") {
      continue;
    }
    clip[`${which}Found`] = runs.length;
    clip[`${which}State`] = "done";
    clip[`${which}Source`] = "cache";
    // Said as a detection that has already happened rather than as one that
    // just did, as a restored commercial detection is.
    clip[`${which}Phase`] = t("flat.previous", {
      note: t(`${which}.rowNote`, { n: runs.length }),
    });
    said = true;
  }
  if (!said) return;
  paintRow(clip);
  paintButtons();
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

/// What the two detections are to judge by, out of 環境設定.
///
/// The same answers the editor asks with -- both windows are the same origin
/// and read the one store -- so a stretch this lane wrote down is a stretch
/// that window will accept from the cache rather than read the recording
/// again. See `flatAsk` in `main.js`.
function flatAsk() {
  const pics = prefs.get("blankRunUnit") === "frame";
  const run = Math.max(0, Number(prefs.get("blankRun")) || 0);
  const quiet = Math.max(0, Number(prefs.get("quietRun")) || 0);
  const shades = prefs.get("blankShades") || "both";
  return {
    minSeconds: pics ? 0 : run,
    minPictures: pics ? Math.max(1, Math.round(run)) : 1,
    // Which shades that pass is to look for. The pass is told, rather than
    // its answer filtered: what goes in the cache is then the question that
    // was asked, and a detection made for black alone does not stand in for
    // one that was asked about white.
    black: shades !== "white",
    white: shades !== "black",
    // ...and what it is to call black, white and enough of the picture. Sent
    // together because they are answered together, and as fractions because
    // that is how the engine holds them: 環境設定 asks in percent.
    levels: prefs.blankLevels(),
    // In seconds whatever it was typed in: the sound has no pictures to
    // count. A list holds recordings of different frame rates, so the
    // conversion is per clip and is made below.
    quietRun: quiet,
    quietInPictures: prefs.get("quietRunUnit") === "frame",
    thresholdDb: Number(prefs.get("quietLevel")) || -50,
  };
}

/// Read one clip for its flat pictures.
///
/// Every picture is decoded, which is the dearest pass this list makes -- a
/// minute and a quarter on half an hour of broadcast, where the walk is
/// seconds. So it is never started by the list on its own: a row gets here
/// because somebody asked for it, and what it finds is written to the cache
/// the editor reads on the way in.
async function runBlank(clip) {
  const ask = flatAsk();
  await runFlatLane(clip, "blank", () =>
    invoke("detect_blank_at", {
      path: clip.path,
      minSeconds: ask.minSeconds,
      minPictures: ask.minPictures,
      black: ask.black,
      white: ask.white,
      levels: ask.levels,
    })
  );
}

/// ...and for its quiet sound, which is the other lane.
///
/// Its own lane because it is its own answer and its own cost: the sound of
/// half an hour is read in seconds off a local disc, and a list asked for the
/// silences alone should not be behind a pass that decodes every picture.
/// Over a share the two come out level -- both read the whole file -- which is
/// why they are allowed to run at once rather than made to take turns.
async function runQuiet(clip) {
  const ask = flatAsk();
  const fps = clip.info && clip.info.fps > 0 ? clip.info.fps : 30;
  await runFlatLane(clip, "quiet", () =>
    invoke("detect_quiet_at", {
      path: clip.path,
      thresholdDb: ask.thresholdDb,
      minSeconds: ask.quietInPictures ? ask.quietRun / fps : ask.quietRun,
    })
  );
}

/// One of the two, on the row's own set of fields.
///
/// The same shape twice, so what a row says about the pictures and what it
/// says about the sound are written by the one piece of code: `blankState` and
/// `quietState` differ in nothing but which pass they are waiting for.
async function runFlatLane(clip, which, call) {
  clip[`${which}State`] = "running";
  clip[`${which}Progress`] = 0;
  clip[`${which}Phase`] = t("phase.detecting");
  paintRow(clip);
  paintQueueNote();
  try {
    const runs = await call();
    clip[`${which}Found`] = runs.length;
    clip[`${which}State`] = "done";
    clip[`${which}Source`] = null;
    clip[`${which}Phase`] = t(`${which}.rowNote`, { n: runs.length });
    // And onto the timeline now if that window is open on this row, which it
    // can be: the lanes do not stand aside for the editor, so a detection can
    // finish while its clip is being cut. That window reads the cache on the
    // way in and does not look again, so what lands afterwards is handed over
    // -- as `runCm` hands a commercial detection over.
    if (clip === editing && emit) emit("flat-found", { id: clip.id, which, runs });
  } catch (e) {
    if (String(e).includes("cancelled")) {
      clip[`${which}State`] = "queued";
      clip[`${which}Phase`] = t("phase.stopped");
    } else {
      clip[`${which}State`] = "error";
      clip[`${which}Phase`] = t("flat.failed", { e });
    }
  }
  paintRow(clip);
  paintButtons();
  paintQueueNote();
  if (clip.selected) paintProps();
}

if (listen) {
  listen("clip-progress", (ev) => {
    const [path, lane, phase, done] = ev.payload;
    // The row that lane is on, not merely the first one holding that path:
    // the same recording can be in the list twice, and the two index lanes
    // run at once on different rows.
    const on = lane === "walk" ? (c) => c.state === "indexing" : (c) => c.pics === "running";
    const clip = clips.find((c) => c.path === path && on(c));
    if (!clip) return;
    clip.phase = phase;
    clip.progress = done;
    paintRow(clip);
  });
  // One event each, the lane being all the row needs to be told: which pass
  // this is, is which listener heard it.
  listen("clip-blank-progress", (ev) => {
    const [path, done] = ev.payload;
    const clip = clips.find((c) => c.path === path && c.blankState === "running");
    if (!clip) return;
    clip.blankProgress = done;
    paintRow(clip);
  });
  listen("clip-quiet-progress", (ev) => {
    const [path, done] = ev.payload;
    const clip = clips.find((c) => c.path === path && c.quietState === "running");
    if (!clip) return;
    clip.quietProgress = done;
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
  // And so is the row the menu was opened over. Every structural change to
  // the list comes through here -- rows added, removed, reordered -- and a
  // menu still standing after one is a menu pointing at nothing.
  closeRowMenu();
  // The field a row was being renamed in is about to be thrown away with the
  // row it sits over, so what is in it is taken first: a name somebody has
  // typed is kept, never lost to a row arriving somewhere else in the list.
  endRename(true);
  const list = el("cliplist");
  list.innerHTML = "";
  for (const clip of clips) {
    const li = document.createElement("li");
    li.className = "clip";
    li.innerHTML = `
      <span class="n"></span>
      <img class="poster" alt="" />
      <div class="nm"></div>
      <div class="meta">
        <div class="sub dim"></div>
        <div class="cm dim"></div>
      </div>
      <div class="stat">
        <div class="badges">
          <span class="cmbadge dbadge" hidden></span>
          <span class="blankbadge dbadge" hidden></span>
          <span class="quietbadge dbadge" hidden></span>
          <span class="editbadge dbadge edited" hidden></span>
          <span class="badge"></span>
        </div>
        <div class="pbar"><span></span></div>
        <div class="ptext dim"></div>
      </div>
      <button class="kill" title="${esc(t("list.kill"))}">×</button>`;
    li.querySelector(".poster").draggable = false;
    li.addEventListener("mousedown", (ev) => {
      if (ev.target.closest(".kill")) return;
      pressRow(clip, ev);
    });
    // Anywhere but the cross. Deleting a run of rows is done by pressing the
    // cross where it is and leaving the pointer there, and the list closes up
    // under it: the next row comes up to meet the pointer and the next press
    // lands on *its* cross. Two of those presses inside the double-click time
    // are a `dblclick` as well as two `click`s, and it is dealt to the row the
    // second press landed on -- the row that press has just deleted, which is
    // out of the list and out of the page by then but still carries this
    // listener. The editor came up on a recording nothing was listed against,
    // and closing it with キャンセル looked like the cancel had deleted the row.
    li.addEventListener("dblclick", (ev) => {
      if (ev.target.closest(".kill")) return;
      edit(clip);
    });
    li.addEventListener("contextmenu", (ev) => {
      ev.preventDefault();
      // The menu is about the selection, and a row nobody had chosen becomes
      // the selection by being right-clicked. A row already in one is left
      // alone: collapsing five rows to the one under the pointer would leave
      // the menu doing its work to a row nobody asked it about.
      if (!clip.selected) pick(clip, {});
      openRowMenu(ev.clientX, ev.clientY);
    });
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

/// A line of a row, with the whole of it a hover away.
///
/// Every line in a row is one line whatever the window is: the name ends in
/// an ellipsis where the episode number should be, and the two lines under it
/// lose the tail of a list of facts -- the codec, what the detections found,
/// how much the edit takes out. All three are worth having in full, and the
/// only room for them is over the row.
///
/// The name alone carried one at first, on the grounds that the lines under
/// it were a list rather than the one thing telling two rows apart. That is
/// true of the *first* fact on each of those lines and not of the last, which
/// is the one being cut off.
function setLine(node, text) {
  setText(node, text);
  if (node.title !== text) node.title = text;
}

/// The picture standing for a clip: the one out of the thumbnail track,
/// taken against the clip's own cuts, and the cheap one from `glance` until
/// there is a track to take the other from.
///
/// On the row rather than in `info`, because `info` is the *file's* answer and
/// duplicates share it -- two cuts of one recording are two pictures.
const posterOf = (clip) => clip.poster || clip.glance || null;

/// What the row shows about the recording: the walk's answer where there is
/// one, and the container's own until then.
///
/// Never the other way round. The two agree on most fields, and where they
/// differ the walk is right: a program stream does not record its own length
/// and libavformat has been seen to work out eight seconds for an hour of
/// DVD, which is a number no row should keep once something better exists.
const factsOf = (clip) => clip.info || clip.outline || null;

/// Give the rows that were just added everything that can be had cheaply:
/// what the container says about each recording, and a picture out of it.
///
/// Both cost one open apiece -- a probe of the head of the file, and a seek
/// and a GOP -- so tens of milliseconds each however long the recording is.
/// The long passes cost a second a gigabyte and four again, and until this
/// existed a row had nothing at all until they had both been over it: a
/// folder of an evening's recordings was a screen of paths and blank
/// rectangles, and the last row stayed that way until the nineteen above it
/// had been read through twice.
///
/// One at a time and lowest row first, in front of nothing: the passes have
/// the cores and this is only worth having while they are still working.
/// Asked once per row -- a recording that will not answer is not asked again
/// every time another one is added.
let firstLooking = false;
async function fillFirstLook() {
  if (firstLooking) return;
  firstLooking = true;
  try {
    for (;;) {
      const clip = clips.find((c) => c.glance === null && !posterOf(c));
      if (!clip) break;
      // The text before the picture: it is what the row is mostly made of,
      // and it is the cheaper of the two.
      if (!clip.outline && !clip.info) {
        try {
          clip.outline = await invoke("clip_outline", { path: clip.path });
          paintRow(clip);
          paintTotals();
          if (clip.selected) paintProps();
        } catch (e) {
          // Not the row's error. A file that will not open is worth saying
          // so about, but the walk behind this says it better and says it in
          // the one place the row shows an error; this is only the first of
          // the two to find out.
          jlog(`clip_outline: ${e}`);
        }
      }
      let url = null;
      try {
        url = await invoke("clip_glance", { path: clip.path });
      } catch (e) {
        jlog(`clip_glance: ${e}`);
      }
      // "" and not null, so that a recording nothing could be decoded out of
      // is asked about once rather than on every pass through the list.
      clip.glance = url || "";
      if (url && !clip.poster) paintRow(clip);
    }
  } finally {
    firstLooking = false;
  }
}

/// Take the row's picture again, against what the cuts leave.
///
/// A tenth of the way into a broadcast recording is as often as not inside the
/// first commercial break, so a clip that has had its commercials cut and
/// still shows one of them is a row lying about what it holds. Nothing is
/// decoded -- the frame comes out of the thumbnail track, from the editor's
/// own copy of it while that window is open -- so this can follow a cut as it
/// is being made.
///
/// Keyed on the ranges it was taken for: `editor-state` arrives on every
/// playhead move, and the picture only changes when the cuts do.
async function refreshPoster(clip) {
  if (!clip.info) return;
  const keeps = rangesOf(clip);
  const sig = JSON.stringify(keeps);
  if (clip.posterSig === sig) return;
  clip.posterSig = sig;
  // Everything cut away: there is no frame of this clip left to show it with,
  // so the last one stands until there is.
  if (!keeps.length) return;
  try {
    const url = await invoke("clip_poster", { path: clip.path, keeps });
    // Nothing where there is no track to take it from, and the row keeps what
    // it had. And the cuts may have moved on while this was in flight, in
    // which case the later answer is the one that speaks for the row.
    if (url && clip.posterSig === sig) {
      clip.poster = url;
      paintRow(clip);
    }
  } catch (e) {
    jlog(`clip_poster: ${e}`);
  }
}

function paintRow(clip) {
  const li = clip.row;
  if (!li) return;
  li.classList.toggle("on", clip.selected);
  li.classList.toggle("bad", clip.state === "error");
  setText(li.querySelector(".n"), String(clips.indexOf(clip) + 1));
  const img = li.querySelector(".poster");
  const poster = posterOf(clip);
  if (poster && img.src !== poster) img.src = poster;
  img.classList.toggle("blank", !poster);
  setLine(li.querySelector(".nm"), clipLabel(clip));

  // Everything on this line is something the container itself knows, so it
  // is filled in from the cheap first look and corrected by the walk. See
  // [`factsOf`].
  //
  // Except on a row that has failed, where the line says why instead. The
  // facts are about a file that cannot be read any more -- a recording that
  // has been renamed keeps every one of them, read when it was still there
  // -- and a red row that goes on reciting its frame rate does not say what
  // is the matter with it.
  const i = factsOf(clip);
  const wrong = clip.state === "error" && clip.error;
  // The length is the *cut's* where there is one; see [`cutLength`]. The
  // frame count with it, since the two are one statement -- a length in
  // minutes and a count of the frames the file holds would be two different
  // clips described in one line.
  const cut = cutLength(clip);
  const runs = cut === null ? (i ? i.duration : 0) : cut;
  setLine(
    li.querySelector(".sub"),
    wrong
      ? clip.error
      : i
        ? t(cut === null ? "row.sub" : "row.subCut", {
            len: coarse(runs),
            frames: cut === null ? i.frames : Math.round(runs * i.fps),
            full: coarse(i.duration),
            end: fmt(runs),
            w: i.width,
            h: i.height,
            fps: i.fps.toFixed(2),
            codec: i.codec,
            audio: i.has_audio ? "" : t("row.noAudio"),
          })
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
  } else if (clip.cmState === "queued") {
    // Two different waits, and the difference is the whole of what somebody
    // wants to know from a list that is still being read: the lane is busy
    // with another row, or this row is not ready to be detected yet.
    bits.push(t(clip.state === "ready" ? "row.cmQueued" : "row.cmReserved"));
  } else if (clip.cmPhase) bits.push(t("row.cmNote", { note: clip.cmPhase }));
  // The two flat detections, a line each. Said apart because they are asked
  // for apart: a row can be part way through the pictures with the sound
  // already answered, and one line for both would have to pick which of the
  // two to be about.
  for (const which of ["blank", "quiet"]) {
    const state = clip[`${which}State`];
    if (state === "running") {
      bits.push(
        t(flatKey(which, `row.${which}Running`), {
          pct: Math.round(clip[`${which}Progress`] * 100),
        })
      );
    } else if (state === "queued") {
      const waiting = clip.state === "ready" ? "Queued" : "Reserved";
      bits.push(t(flatKey(which, `row.${which}${waiting}`)));
    } else if (clip[`${which}Phase`]) {
      bits.push(t(flatKey(which, `row.${which}Note`), { note: clip[`${which}Phase`] }));
    }
  }
  const cutCount = clip.edit ? clip.edit.cuts.length : 0;
  // How many, and no longer what they leave: the line above is the length of
  // what they leave.
  if (cutCount && i) bits.push(t("row.cuts", { n: cutCount }));
  if (clip.edit && clip.edit.keyframes.length) {
    bits.push(t("row.keyframes", { n: clip.edit.keyframes.length }));
  }
  setLine(li.querySelector(".cm"), bits.join(t("sep")));

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

  // And what each detection has to say, which the line under the name says
  // already but only to someone reading it. A list of twenty recordings is
  // scanned, not read, and the one thing being looked for in that scan is
  // which of them still owe an answer -- so it goes where the eye is already
  // going, beside the state badge.
  //
  // The commercials' count is the editor's where the clip has been through
  // it, because that is what the timeline actually holds; otherwise it is the
  // finding's own. The two flat detections have no such second copy: what
  // they found is in the cache both windows read, and a row comes back from
  // the editor by asking that cache again (`restoreFlat`).
  //
  // A detection that has been asked for and not yet made is a badge too.
  // Nothing was shown for one while the only way to ask was to ask a row the
  // walk had finished -- the bar underneath was already saying it, and it
  // was saying it about a pass that was about to start. A booked detection
  // on a list still being read is a different thing: it can sit there for a
  // quarter of an hour behind eighteen other recordings, and what somebody
  // scanning the list wants to know is which rows are spoken for.
  //
  // **A row goes on wearing it while its detection runs.** The badge answers
  // "does this row still owe me an answer", and a pass that is halfway
  // through has not given one. It was hidden there at first, on the grounds
  // that the bar underneath was saying it -- but the bar serves one pass and
  // there are three lanes. The pictures pass lands on a row the moment its
  // walk finishes, which is the same moment the detection lane can take it,
  // so the two are on the same row constantly and the bar goes to the
  // pictures. The badge would vanish exactly when the row got busy, which is
  // the one time a list is worth scanning.
  //
  // Three looks, then, and the eye can sort them without reading: dashed is
  // owed, solid is being worked on, filled is an answer. The percentage and
  // the phase stay in the line under the name, where there is room for them.
  //
  // One badge per detection, and the three are the same badge: the question
  // is the same question three times over, and an answer that had to be told
  // from the others by its colour would be an answer somebody has to read.
  const blocks =
    clip.edit && clip.edit.cmBlocks
      ? clip.edit.cmBlocks.length
      : clip.cm
        ? clip.cm.blocks.length
        : null;
  paintDetectBadge(li.querySelector(".cmbadge"), clip.cmState, blocks, "cm");
  paintDetectBadge(li.querySelector(".blankbadge"), clip.blankState, clip.blankFound, "blank");
  paintDetectBadge(li.querySelector(".quietbadge"), clip.quietState, clip.quietFound, "quiet");

  // And whether the row has been settled in the cut editor, which is the one
  // thing about it that nothing else on the row says. A list read overnight
  // comes back with marks and blocks on every row: what cannot be seen by
  // looking at it is which of them somebody has since been through.
  //
  // Not while that window is open on it -- the state badge beside this one is
  // saying 編集中, and a row cannot usefully be both.
  const done = li.querySelector(".editbadge");
  done.hidden = !clip.edited || clip === editing;
  if (!done.hidden) {
    setText(done, t("badge.edited"));
    const tip = t("badge.editedTitle");
    if (done.title !== tip) done.title = tip;
  }

  // The bar serves whichever of the three passes is on this row. The two in
  // the index lane share `progress` because only one of them can be running
  // -- they are the same lane -- and `phase` says which it is.
  //
  // More than one of them can be running on the one row -- the sound and the
  // pictures are separate lanes -- and there is one bar. It shows the pass
  // that is furthest from done, which is the one the row is still waiting for.
  const mine = clip.state === "indexing" || clip.pics === "running";
  const others = [
    ["blank", clip.blankState === "running", clip.blankProgress],
    ["quiet", clip.quietState === "running", clip.quietProgress],
    ["cm", clip.cmState === "running", clip.cmProgress],
  ].filter(([, on]) => on);
  others.sort((a, b) => a[2] - b[2]);
  const running = mine || others.length > 0;
  const pct = mine ? clip.progress : others.length ? others[0][2] : 0;
  li.querySelector(".pbar").hidden = !running;
  li.querySelector(".pbar span").style.width = `${Math.round(pct * 100)}%`;
  // Whose percentage the bar is showing, in the words of the lane it belongs
  // to. The row's own phase where the bar is the walk's or the pictures', and
  // the lane's name otherwise.
  const whose =
    mine && clip.phase ? clip.phase : others.length ? t(`ptext.${others[0][0]}`) : clip.phase;
  setText(
    li.querySelector(".ptext"),
    running
      ? t("ptext.running", { phase: whose, pct: Math.round(pct * 100) })
      : clip.state === "queued"
        ? t("phase.queued")
        : clip.phase
  );
}

function paintTotals() {
  // The container's own length counts here. It is right on everything but a
  // program stream and the walk corrects it there, and a total that waits
  // for every walk to finish says "未解析 20 本を除く" over a list whose
  // every row is already showing its length.
  const known = clips.filter((c) => factsOf(c));
  // What the rows add up to, which is what the rows say: the cut's length
  // where the clip has been cut, and the file's where it has not. The file's
  // total is still worth having beside it -- it is what was recorded, and the
  // difference between the two is the evening's work -- so it follows in
  // brackets, and only where something has actually been cut.
  const total = known.reduce((n, c) => n + (cutLength(c) ?? factsOf(c).duration), 0);
  const full = known.reduce((n, c) => n + factsOf(c).duration, 0);
  const pending = clips.length - known.length;
  el("clip-total").textContent =
    t(full - total > 0.5 ? "input.totalCut" : "input.total", {
      n: clips.length,
      t: coarse(total),
      full: coarse(full),
    }) + (pending ? t("input.totalPending", { n: pending }) : "");
}

/// One detection's badge on one row: booked, being made, or an answer.
///
/// `found` is how many it found, or null where this detection has never been
/// made against this recording -- which is not the same as zero, and is the
/// difference between no badge at all and a badge reading 黒白なし.
function paintDetectBadge(span, state, found, which) {
  const owed = state === "queued" || state === "running";
  const answered = state === "done" && found !== null && found !== undefined;
  span.hidden = !owed && !answered;
  if (owed) {
    const run = state === "running";
    setText(span, t(flatKey(which, `badge.${which}${run ? "Running" : "Queued"}`)));
    span.className = `${which}badge dbadge ${run ? "detecting" : "queued"}`;
  } else if (answered) {
    setText(
      span,
      found
        ? t(flatKey(which, `badge.${which}`), { n: found })
        : t(flatKey(which, `badge.${which}None`))
    );
    span.className = `${which}badge dbadge ${found ? "found" : "empty"}`;
  }
}

/// What the clip commands could do to what is chosen right now.
///
/// One answer for the two places that ask -- the buttons down the side and
/// the menu on the right button -- because a command that is grey in one of
/// them and live in the other is the program disagreeing with itself about
/// what it is willing to do.
function clipActions() {
  const picked = selected();
  return {
    // Anything but a clip that could not be read: the editor makes its own
    // way through one the list has not got to yet.
    edit: picked.length === 1 && picked[0].state !== "error",
    // Any one row, whatever state it is in: what a row is called is the
    // list's own answer and does not wait on a pass over the recording.
    rename: picked.length === 1,
    duplicate: picked.length > 0,
    // Anything the detection lane could take now or later, which is
    // everything but a recording that could not be read; see
    // `detectSelected`.
    detect: picked.some((c) => c.state !== "error" && c.cmState !== "running"),
    detectBlank: picked.some((c) => c.state !== "error" && c.blankState !== "running"),
    detectQuiet: picked.some((c) => c.state !== "error" && c.quietState !== "running"),
    move: picked.length > 0,
    remove: picked.length > 0,
  };
}

function paintButtons() {
  const busy = running();
  const can = clipActions();
  el("edit-clip").disabled = !can.edit;
  el("rename-clip").disabled = !can.rename;
  el("duplicate-clip").disabled = !can.duplicate;
  el("detect-selected").disabled = !can.detect;
  el("detect-blank-selected").disabled = !can.detectBlank;
  el("detect-quiet-selected").disabled = !can.detectQuiet;
  const queued = clips.some(
    (c) =>
      c.state === "queued" ||
      c.pics === "queued" ||
      c.cmState === "queued" ||
      c.blankState === "queued" ||
      c.quietState === "queued"
  );
  el("stop-batch").disabled = !busy && !(paused && queued);
  el("stop-batch").textContent = t(paused && queued && !busy ? "side.resumeBatch" : "side.stopBatch");
  el("move-up").disabled = !can.move;
  el("move-down").disabled = !can.move;
  el("select-all").disabled = clips.length === 0;
  el("remove-clip").disabled = !can.remove;
  el("remove-all").disabled = clips.length === 0;
  paintExportButton();
  paintEnlistButton();
  // The queue drives this screen, so what it can do changes with it.
  if (el("batch-go")) paintBatchButtons();
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
  const i = factsOf(c);
  if (!i) {
    box.textContent =
      c.state === "error"
        ? t("props.error", { name: clipName(c), error: c.error })
        : t("props.queued", { name: clipName(c) });
    return;
  }
  // Three of the lines here are the walk's alone -- where the lossless points
  // are, how many cannot start a cut, which index the answer came from -- and
  // a fourth is the picture pass's. Shown as not yet known rather than as
  // zero: "無劣化点 0 個" about a recording that has plenty is worse than
  // saying nothing, and this panel is up while the passes are still running.
  const walked = !!c.info;
  const pending = t("props.pending");
  const flags = [
    t(i.interlaced ? "media.interlaced" : "media.progressive"),
    // Only the walk can see pulldown -- it is a flag on the pictures, not in
    // the container -- so before it has run this says nothing rather than
    // saying the recording is free of it.
    walked && i.pulldown ? t("media.pulldown") : null,
    walked && i.variable ? t("media.variable") : null,
  ]
    .filter(Boolean)
    .join(", ");
  const n = copyNo(c);
  const sound = audioOf(c);
  box.textContent = t("props.body", {
    name: clipName(c),
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
    // The count belongs to the track this clip keeps, not to whichever one
    // the demuxer thinks is the main one. See [`audioOf`].
    audio: sound
      ? `${t("media.audioYes")}${sound.channels ? ` (${chLabel(sound.channels)})` : ""}`
      : t("media.audioNo"),
    len: coarse(i.duration),
    frames: i.frames,
    // The file's own length, and what the cut leaves where one has been
    // made. This panel is the one place that says both: the row says the
    // length the clip now has, which is the one being worked to.
    cut: cutLength(c) === null ? "" : t("props.cut", { len: coarse(cutLength(c)) }),
    points: walked ? i.points : pending,
    unusable:
      walked && i.unusable_points ? t("props.unusable", { n: i.unusable_points }) : "",
    scenes: c.scenes === null ? pending : c.scenes,
    index: walked ? i.index_name : pending,
    cm: c.cmPhase ? t("props.cm", { note: c.cmPhase }) : "",
    // A line each, and only where there is something to say: a row that has
    // never been asked for either of them says nothing about them, as it
    // says nothing about commercials.
    flat:
      (c.blankPhase ? t(blankKey("props.blank"), { note: c.blankPhase }) : "") +
      (c.quietPhase ? t("props.quiet", { note: c.quietPhase }) : ""),
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

// A press on the list where there is no row is a press on nothing, and that
// is an answer rather than an accident: it lets twenty selected rows go
// without having to find one of them to click on. The right button is left
// out -- it opens the menu, and a menu about nothing is not worth clearing a
// selection for.
el("droptarget").addEventListener("mousedown", (ev) => {
  if (ev.button !== 0 || ev.target.closest(".clip")) return;
  if (!selected().length) return;
  clips.forEach((c) => (c.selected = false));
  anchor = -1;
  paintList();
});

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
      // A pass in flight belongs to the row it was started on. The copy has
      // none, so it takes its place in the queue rather than inheriting a
      // state that nothing is ever going to finish.
      state: src.state === "indexing" ? "queued" : src.state,
      pics: src.pics === "running" ? "queued" : src.pics,
      cmState: src.cmState === "running" ? "queued" : src.cmState,
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
  // Out of the list before anything is asked of the other side.
  //
  // The rows used to go after the passes had been told to stop, and every one
  // of those is a trip over the wire and back. They are answered as fast as
  // anything here is -- a counter is raised, and the pass reads it when it
  // next comes up for air -- but "as fast as anything here is" is not the same
  // as "before the next press", and while the list is reading a folder of
  // recordings it is not close. So a row being read stayed where it was for
  // as long as the round trip took, with the cross still under the pointer,
  // and a second press in that time was a press on the row that was already
  // going: it deleted nothing of its own, and the two presses together were a
  // `dblclick` on a row that then went. The editor came up on a recording the
  // list no longer had, and closing it with キャンセル looked like the cancel
  // had deleted the row. Nothing about the list's own bookkeeping needs the
  // passes to have heard first, so it does not wait for them: a press takes a
  // row out while the finger is still on the button, and every press after it
  // is about a row that is still there.
  clips = clips.filter((c) => !gone.has(c.id));
  anchor = -1;
  renderList();
  // And now the passes. A clip being read right now has one behind it that
  // has to be told to stop, or it would go on reading a file nothing is
  // listed against. Only the lanes that are on one of these clips: the others
  // are working on clips that are staying, and the lanes are stopped apart
  // for that reason. Before `pump`, which is what starts the next pass --
  // a stop raised after that would put the new one down with the old.
  if (doomed.some((c) => c.state === "indexing")) {
    await invoke("stop_batch", { lane: "walk" });
  }
  if (doomed.some((c) => c.pics === "running")) {
    await invoke("stop_batch", { lane: "pics" });
  }
  if (doomed.some((c) => c.cmState === "running")) {
    await invoke("stop_batch", { lane: "cm" });
  }
  if (doomed.some((c) => c.blankState === "running")) {
    await invoke("stop_batch", { lane: "blank" });
  }
  if (doomed.some((c) => c.quietState === "running")) {
    await invoke("stop_batch", { lane: "quiet" });
  }
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
el("detect-blank-selected").addEventListener("click", () => detectFlatSelected("blank"));
el("detect-quiet-selected").addEventListener("click", () => detectFlatSelected("quiet"));
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

// --- 名前の変更 ----------------------------------------------------------
//
// A row is named after the file it came off, and for a broadcast recording
// that name carries the date, the channel and the episode -- everything you
// would need to find it again, which is why it is kept. It is not always the
// name the *cut* wants: twelve recordings that are about to be twelve episodes
// of one thing are a list whose useful names are 第1話 … 第12話, and nothing in
// any of the files says so.
//
// So the name can be typed over, in the row itself rather than in a dialog:
// F2 and a field where the name already is, which is what a file manager does
// and where the eye already is. What is typed is the row's name everywhere the
// row is named -- the list, the quick properties, the cut editor's header, the
// file a cut of it is written to, and the programme on a disc's index unless
// the output screen has been told otherwise.

/// The row being renamed and the field it is being renamed in, or null.
let renaming = null;

/// Put a field over a row's name, with the name in it.
///
/// The row's own `.nm` line is hidden rather than replaced: it is what
/// `paintRow` writes to, and a repaint arriving mid-rename -- the walk
/// finishes, a detection reports -- must not have to know this is going on.
function startRename(clip) {
  if (!clip.row) return;
  // Whatever was being renamed is settled first. Two fields open at once is
  // two answers, and the one being left is the one somebody has finished with.
  endRename(true);
  const line = clip.row.querySelector(".nm");
  if (!line) return;
  const field = document.createElement("input");
  field.type = "text";
  field.className = "rename";
  field.spellcheck = false;
  // The name alone. What the row *shows* can carry a copy number as well --
  // `録画.ts（2）` -- and that is the list telling two rows on one recording
  // apart, not part of what either of them is called.
  field.value = clipName(clip);
  line.hidden = true;
  line.parentNode.insertBefore(field, line);
  renaming = { clip, field };
  // The row may have been reached with the arrow keys, from off the screen.
  clip.row.scrollIntoView({ block: "nearest" });
  field.focus();
  field.select();
  // A press in the field is not a press on the row: the row would take it as
  // the beginning of a drag, and the double click that selects a word in it
  // would open the cut editor.
  for (const kind of ["mousedown", "dblclick", "click"]) {
    field.addEventListener(kind, (ev) => ev.stopPropagation());
  }
  field.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") {
      ev.preventDefault();
      endRename(true);
    } else if (ev.key === "Escape") {
      ev.preventDefault();
      endRename(false);
    }
    // Everything else is typing, and it stops here: the list's own keys are on
    // the window, where Delete would take the row out from under the field.
    ev.stopPropagation();
  });
  // Clicking away keeps it. The other way round -- a name thrown away by a
  // click that landed somewhere else -- is the one outcome that loses work.
  field.addEventListener("blur", () => endRename(true));
}

/// Take the field away, keeping what is in it or not.
///
/// A name that is empty, or that is the one the row arrived with, is not a
/// rename: it is `renamed` being null again. So emptying the field undoes the
/// rename rather than settling on a row called nothing.
function endRename(keep) {
  if (!renaming) return;
  const { clip, field } = renaming;
  // Let go of before the field does, because taking a focused field out of the
  // page is a blur arriving straight back in here.
  renaming = null;
  const typed = keep ? field.value.trim() : null;
  field.remove();
  const line = clip.row && clip.row.querySelector(".nm");
  if (line) line.hidden = false;
  if (typed === null) return;
  const was = clip.renamed;
  clip.renamed = typed && typed !== clip.name ? typed : null;
  if (clip.renamed === was) return;
  paintRow(clip);
  paintProps();
  // A name is half of what a cut is written as, so both output screens were
  // showing the old one.
  renderOutset();
  renderOutScreen();
  // And the cut editor, if this row is open in it: the name is in its title
  // bar and on its own header, and neither is something that window can work
  // out for itself.
  if (clip === editing) {
    invoke("retitle_editor", { title: t("editor.windowTitle", { clip: clipLabel(clip) }) });
    if (emit) emit("clip-renamed", { id: clip.id, name: clipName(clip) });
  }
  touch();
}

/// Rename whatever single row is selected. What F2, the button down the side
/// and the item on the right button's menu all come to.
function renameSelected() {
  const one = selected();
  if (one.length === 1) startRename(one[0]);
}

el("rename-clip").addEventListener("click", renameSelected);

// --- the menu on the right button ---------------------------------------
//
// The clip commands again, under the pointer instead of down the side. Only
// the ones that are about a clip: ファイルを追加, 全選択, 全削除 and
// 解析を中止 are about the list or about the program, and a row is not what
// they would be answering.
//
// Each item does exactly what the button of the same name does, by calling
// the same function -- there is no second version of 複製 or of 削除 here to
// drift away from the first.

const rowMenu = el("row-menu");

// A declaration rather than a `const`, because `show` and `renderList` are
// above this and both put the menu away.
function closeRowMenu() {
  rowMenu.hidden = true;
}

/// Put the menu up at the pointer, greyed to what the selection allows.
function openRowMenu(x, y) {
  const can = clipActions();
  el("row-edit").disabled = !can.edit;
  el("row-rename").disabled = !can.rename;
  el("row-duplicate").disabled = !can.duplicate;
  el("row-detect").disabled = !can.detect;
  el("row-detect-blank").disabled = !can.detectBlank;
  el("row-detect-quiet").disabled = !can.detectQuiet;
  el("row-up").disabled = !can.move;
  el("row-down").disabled = !can.move;
  el("row-remove").disabled = !can.remove;
  // The one in the corner is a menu too, and two menus standing at once is
  // one of them left over from a click that was meant for something else.
  showMenu(false);
  rowMenu.style.left = `${x}px`;
  rowMenu.style.top = `${y}px`;
  rowMenu.hidden = false;
  // Measured only now: a menu is as wide as its longest label, and the
  // labels are not in it until the language is. Held inside the window on
  // both axes, so a right click near the bottom edge does not open a menu
  // whose last item is off the screen.
  const box = rowMenu.getBoundingClientRect();
  const left = Math.max(0, Math.min(x, window.innerWidth - box.width - 2));
  const top = Math.max(0, Math.min(y, window.innerHeight - box.height - 2));
  rowMenu.style.left = `${left}px`;
  rowMenu.style.top = `${top}px`;
}

el("row-edit").addEventListener("click", () => {
  closeRowMenu();
  const one = selected();
  if (one.length === 1) edit(one[0]);
});
el("row-rename").addEventListener("click", () => {
  closeRowMenu();
  renameSelected();
});
el("row-duplicate").addEventListener("click", () => {
  closeRowMenu();
  duplicate(selected());
});
el("row-detect").addEventListener("click", () => {
  closeRowMenu();
  detectSelected();
});
el("row-detect-blank").addEventListener("click", () => {
  closeRowMenu();
  detectFlatSelected("blank");
});
el("row-detect-quiet").addEventListener("click", () => {
  closeRowMenu();
  detectFlatSelected("quiet");
});
el("row-up").addEventListener("click", () => {
  closeRowMenu();
  move(-1);
});
el("row-down").addEventListener("click", () => {
  closeRowMenu();
  move(1);
});
el("row-remove").addEventListener("click", () => {
  closeRowMenu();
  remove(selected());
});

// A press anywhere else shuts it -- `mousedown` rather than `click`, because
// the press that opened this one was a right button and a right button
// elsewhere is a click that never arrives. Not on the menu itself: hiding it
// under the finger would take the button out from under the click that was
// about to land on it.
window.addEventListener("mousedown", (ev) => {
  if (!ev.target.closest("#row-menu")) closeRowMenu();
});
window.addEventListener("wheel", closeRowMenu, true);
window.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") closeRowMenu();
});

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
  // The right button's selection belongs to the menu it is opening -- see the
  // `contextmenu` handler on the row -- and settling it here on the way down
  // would collapse the very selection that menu is about. A middle click is
  // not a selection at all.
  if (ev.button !== 0) return;
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

/// Reserve one of the two flat detections on every selected row, as
/// `detectSelected` does for the commercials. A row the walk has not reached
/// waits there rather than being refused: see `nextFor`.
///
/// One lane at a time, `which` being "blank" or "quiet". Asking for both is
/// two presses, which is the point of their being two buttons: the pictures
/// take a minute a recording and the sound does not.
function detectFlatSelected(which) {
  const want = selected().filter((c) => c.state !== "error" && c[`${which}State`] !== "running");
  if (!want.length) return;
  want.forEach((c) => {
    c[`${which}State`] = "queued";
    c[`${which}Phase`] = "";
  });
  paused = false;
  paintList();
  pump();
}

/// Queue a commercial detection on every selected clip that can take one.
///
/// Queued rather than run: the passes are minutes each on a broadcast
/// recording, and the detection lane takes them one at a time. Selecting
/// eighteen clips and pressing Ctrl+D is a night's work asked for in one
/// keystroke, which is the point of it -- and the indexing of the ones still
/// unread carries on beside it.
///
/// Including the ones still being read. A detection cannot start on a row
/// the walk has not finished, so it is queued there and taken when the walk
/// hands the row over; the lane works down the list either way. This used to
/// take only the rows that were ready at the moment the key was pressed,
/// which on a list just dropped in is one or two of them -- and the other
/// sixteen were dropped without a word, so the thing 全選択 → Ctrl+D is for
/// only worked if you waited for the whole list to be read first.
function detectSelected() {
  const want = selected().filter((c) => c.state !== "error" && c.cmState !== "running");
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
///
/// Empty for a clip nothing is known about at all. Before the walk the first
/// access point is not one of the things known -- only the walk finds those
/// -- so the ranges start at zero and are the container's length: near enough
/// for the one thing that asks this early, which is where to take the row's
/// picture from.
function keepsOf(clip) {
  const facts = factsOf(clip);
  if (!facts) return [];
  const dur = facts.duration;
  const keeps = [];
  let pos = clip.info ? clip.info.first_point : 0;
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

/// How long the cut of a clip runs, or `null` where nothing has been cut out
/// of it.
///
/// What the row shows as the clip's length, because that is the length the
/// clip now has: a two-hour recording with the commercials taken out is an
/// hour and a half, and a list still reading 2 時間 8 分 against every row of
/// an evening's work says nothing about the evening. The file's own length is
/// still on the row, behind カット前, and in the properties panel.
///
/// `null` rather than the file's length, so that every caller has to decide
/// what to say about a clip nobody has cut -- which is not "the cut is the
/// same length", it is that there is no cut to be about.
function cutLength(clip) {
  if (!clip.edit || !clip.edit.cuts.length) return null;
  if (!factsOf(clip)) return null;
  return keepsOf(clip).reduce((n, k) => n + (k.b - k.a), 0);
}

function srcToOut(keeps, s) {
  for (const k of keeps) {
    if (s >= k.a - 1e-9 && s < k.b - 1e-9) return k.at + (s - k.a);
  }
  return null;
}

// --- output settings ----------------------------------------------------

const settings = {
  /// Which of the two things a run produces: files, or a disc of recordings.
  ///
  /// One answer for the whole list, like every other setting on the screen:
  /// a disc is one disc, and half a list written onto it and half beside it
  /// is not something anybody asked for.
  mode: "file",
  dir: "",
  /// A folder of its own under `dir`, or "" for none.
  ///
  /// `null` means nobody has settled it yet: the screen fills it in the
  /// first time it is drawn -- the disc's name, or the project's -- and
  /// leaves it a plain field afterwards. That is why "none" is an empty
  /// string rather than a null: a field somebody has emptied on purpose has
  /// been settled, and filling it back in would be arguing.
  subfolder: null,
  /// What the disc is called -- the name a recorder shows over the list of
  /// what is on it. Only means anything in `bdav` mode; empty is filled in
  /// from the first recording when the screen is drawn.
  discTitle: "",
  /// Whether the finished disc is wrapped in an image, and to which UDF
  /// revision: "" for a folder and nothing else, "2.50" or "2.60" for one.
  /// The folder is written either way -- the image is made of it.
  image: "",
  /// What the image says may be done to the disc it is burned onto:
  /// "read-only", which is what a burned disc is, or "overwritable", which is
  /// what a recorder writes on a BD-RE and what it wants to see before it
  /// will edit a disc. Only means anything where an image is being made.
  imageAccess: "read-only",
  /// And whether the folder goes once the image has been made of it. Only
  /// means anything where an image is being made at all, and only ever
  /// happens after one was written: the folder is what the image is made of.
  imageOnly: false,
  prefix: "cut_",
  /// Whether the row's place in the list goes into the name behind the
  /// prefix, and in how many digits.
  ///
  /// The order of a list is an answer somebody gave -- two halves of a film in
  /// the order they are played, twelve episodes in the order they are watched
  /// -- and a folder sorted by name is where that answer is otherwise lost.
  /// The digits are a string because that is what the control holding them
  /// hands back; `seqNo` is where it becomes a number again.
  ///
  /// On, the same as the preference it starts from: the order of a list is
  /// worth keeping more often than not.
  number: true,
  digits: "2",
  container: "",
  audio: "smart",
  /// Empty writes the recording's own codec back; anything else is a
  /// conversion, which like a downmix decides the mode instead of living
  /// under it -- there is no copying a frame into a codec it is not in.
  audioCodec: "",
  /// Empty follows the recording; anything else is a downmix, which is the
  /// one audio setting that decides the mode instead of living under it.
  audioChannels: "",
  /// Empty lets the engine derive one from the recording -- and bring it down
  /// with the channel count when there is a fold.
  audioBitrate: "",
  /// Empty follows the recording; anything else is a resample, which like a
  /// downmix decides the mode instead of living under it -- samples on a
  /// different grid are samples no frame of the recording's can be copied
  /// alongside.
  audioRate: "",
  /// Empty follows the recording. Only reaches the cut where リニア PCM is
  /// what is being written: a lossy encoder takes a float and spends a
  /// bitrate, and how many bits the sound had before it is not a number it
  /// has anywhere to put.
  audioBits: "",
  keyframes: false,
  // Where the subtitles a disc draws go. See `outset.subtitles`.
  subtitles: "pgs",
  /// Which disc the list is going onto, in bytes and as a string -- which is
  /// what the control holding it hands back. Only means anything in `bdav`
  /// mode. The four sizes are in `index.html` beside the control, and in
  /// `smartcut_core::fit::DISCS` where the arithmetic is done.
  disc: "25025314816",
  /// Whether a list that will not fit on that disc is made to fit, by writing
  /// every picture back at a share of its own size.
  ///
  /// Off. A run that quietly rewrote every picture of a night's recordings
  /// because they came to a hundred megabytes more than a disc holds would
  /// be doing something nobody asked for to something that cannot be undone
  /// -- and the alternative, which is a disc with one recording left off it,
  /// is a thing somebody might well prefer. So it is asked for.
  fit: false,
  /// Whether the list is written as one file rather than one per row.
  ///
  /// Off, and deliberately: the list has always been a list of *outputs*,
  /// and a program that quietly made one file out of twenty recordings
  /// because somebody added them in one sitting would be answering a
  /// question nobody asked. What it costs to turn on is one box.
  joinAll: false,
  /// Which clip the joined file takes its shape from, by row id.
  ///
  /// Null is the first row, which is the answer nobody has to think about
  /// and the one a list of recordings off one recorder wants. A row id
  /// rather than a position, because the list can be reordered under it and
  /// the answer is about a recording. See `smartcut_core::conform`.
  master: null,
};

/// The settings as the program starts with them, kept because 新規作成 has to
/// put them back. A project carries its output settings, so starting a new
/// one from the last one's folder and prefix would be starting it half open.
const SETTING_DEFAULTS = { ...settings };

/// The four of them 環境設定 answers for, put into force.
///
/// What a cut is called is the one output setting that is as much about the
/// person as about the work: somebody who writes `編集_` in front of every file
/// writes it in front of the next one too. So the prefix and the number behind
/// it have a standing answer in 環境設定 as well as the per-project one here,
/// and this is where the standing answer becomes the project's -- at the start,
/// on 新規作成, and on 既定に戻す, which is every moment the defaults are what
/// is in force.
///
/// The sound the output is written with is the fourth, and it is the same
/// kind of answer: somebody who writes every cut down to stereo does it to
/// the next one too, and「入力と同じ」-- which is what the screen starts at
/// -- is only the right answer for somebody who never downmixes at all.
///
/// A project saved with its own answers is not one of those moments:
/// `loadProject` writes what the file says over all four.
function applyNameDefaults() {
  settings.prefix = String(prefs.get("outPrefix") ?? SETTING_DEFAULTS.prefix);
  settings.number = !!prefs.get("outNumber");
  settings.digits = String(Number(prefs.get("outDigits")) || 2);
  settings.audioChannels = String(prefs.get("outAudioChannels") ?? "");
  // A count can only be delivered by writing the sound afresh -- copying is
  // copying, and the smart path splices the recording's own frames -- so the
  // mode comes with it. Without this the preference was a control that did
  // nothing: the row it fills is on screen only under すべて再エンコード, and
  // somebody who has asked for stereo has asked for the thing that produces
  // stereo. 入力と同じ leaves the mode alone, that being no request at all.
  if (settings.audioChannels) settings.audio = "reencode";
}

/// Which of them are worth carrying from one session to the next.
///
/// Everything that describes the output rather than the recordings it is
/// made from. The two left out belong to whatever is in the list: the disc's
/// name is read off the first recording, and `subfolder` is null until the
/// screen has settled it. Restoring a stale answer to either would be
/// answering for a list this session has not seen.
const KEPT_SETTINGS = Object.keys(SETTING_DEFAULTS)
  .filter((key) => !["discTitle", "subfolder", "master"].includes(key));

/// The settings as they are now, put away for the next start.
///
/// Written on every change rather than at quit: a program that is killed, or
/// that falls over, has still been used, and the answers it was being used
/// with are the ones worth having back. Stored whatever the preference says
/// -- what it governs is whether they are read again -- so that turning it
/// on has something to restore without waiting for the next change.
function rememberOutput() {
  const kept = {};
  for (const key of KEPT_SETTINGS) kept[key] = settings[key];
  prefs.set("output", kept);
  paintKeptOutput();
}

/// Put a remembered set back, for the start of a session and for 新規作成.
///
/// Only names this build knows, and only where the stored value is the shape
/// this build expects: the store outlives a version, and a setting that has
/// since changed what it means is better left at its default than restored
/// into a control that cannot hold it. `showSettings` does the rest -- it is
/// the one that puts them on screen and reads them back off the controls.
function restoreOutput() {
  if (!prefs.get("keepOutput")) return false;
  const kept = prefs.get("output");
  if (!kept || typeof kept !== "object") return false;
  let any = false;
  for (const key of KEPT_SETTINGS) {
    const value = kept[key];
    if (value === undefined) continue;
    if (typeof value !== typeof SETTING_DEFAULTS[key] && SETTING_DEFAULTS[key] !== null) continue;
    settings[key] = value;
    any = true;
  }
  return any;
}

/// The path a sidecar of this clip has, without the extension.
function sidecarBase(clip) {
  const dir = clip.home ? `${clip.home.replace(/[/\\]*$/, "")}/` : dirOf(clip.path);
  return `${dir}${clip.stem || stemOf(clip.path)}`;
}

/// Bytes, which is what a filesystem counts a name in. One encoder rather
/// than one per character: this runs per character of every name on screen.
const utf8 = new TextEncoder();

/// The characters a filesystem will not take, and what they become.
const FULLWIDTH = {
  "\\": "＼", "/": "／", ":": "：", "*": "＊", "?": "？",
  '"': "＂", "<": "＜", ">": "＞", "|": "｜",
};

/// A name somebody typed, as a file name.
///
/// The same turn `filename` in `disc.rs` does to a programme name off a disc,
/// and for the same reason: the characters a filesystem will not take become
/// their full width forms rather than being dropped, because `第1話？` still
/// reads and `第1話` is a different name. A Japanese recorder does this with
/// the same problem.
///
/// Empty for a name that was nothing but those -- or that was too long to
/// have a first character, which cannot happen -- and the caller falls back
/// to what the recording is called. The limit is in bytes, because that is
/// what a filesystem counts, and a title in Japanese is three bytes a
/// character.
function fileSafe(name) {
  const LIMIT = 180;
  let out = "";
  let used = 0;
  for (const c of String(name)) {
    const put = FULLWIDTH[c] ?? (c.codePointAt(0) < 0x20 ? " " : c);
    const cost = utf8.encode(put).length;
    if (used + cost > LIMIT) break;
    out += put;
    used += cost;
  }
  // Windows will not have a name that ends in a dot or a space, and no
  // filesystem is improved by one.
  return out.replace(/^[\s.\u3000]+|[\s.\u3000]+$/g, "");
}

/// What a cut of this clip is called, before the prefix and the number.
///
/// What somebody renamed the row to, where they did: a rename is about the cut
/// as much as about the row, and a list renamed 第1話 … 第12話 that went on
/// writing twelve files named after the transponder would be a rename that
/// never reached the place it matters. Then the disc's answer, and then the
/// recording's own file name.
function outStem(clip) {
  return fileSafe(clip.renamed || "") || clip.stem || stemOf(clip.path);
}

/// The row's place in the list, as a file name carries it -- `03_`, or "" where
/// nobody asked for one.
///
/// Counted off the list rather than stored, so it is the number on the row:
/// move a row and its file is renumbered with it. Every row counts, the ones
/// still being read included -- the number beside a row and the number in its
/// file name have to be the same number, and a list whose third row wrote
/// `02_` because the second would not open is a list to be checked against the
/// folder afterwards.
///
/// The underscore is the program's rather than a setting: a number run into the
/// name (`cut_03第1話`) is the one shape of this nobody wants, and the prefix
/// is where a different separator can be typed.
function seqNo(clip) {
  if (!settings.number) return "";
  const digits = clamp(Number(settings.digits) || 2, 1, 6);
  return `${String(clips.indexOf(clip) + 1).padStart(digits, "0")}_`;
}

/// Where a clip will be written, given the settings.
///
/// Named after the recording it came from, in the folder chosen or beside
/// it. A broadcast file's name carries the date, the channel and the episode
/// -- everything you would need to find it again -- so throwing it away for
/// "cut.ts" is a loss. The prefix is what says which one is the edit.
///
/// The number in front, where it was asked for, is the row's place in the list
/// -- what the order of the list means is not recoverable from a folder sorted
/// by name otherwise. See `seqNo`.
///
/// Duplicated clips are numbered `_1`, `_2` in list order, because they came
/// from one recording and would otherwise be one filename written twice --
/// the second cut landing on top of the first. Counted off the list rather
/// than fixed at the moment of duplication, so deleting one copy gives the
/// survivor its plain name back.
/// A `.m2ts` is a transport stream in Blu-ray's clothing: the same packets
/// with four bytes of arrival time in front of each, and Blu-ray's own PID
/// numbering. It carries everything a `.ts` carries, the recording's own
/// tables included -- it is what a recording written onto a disc is -- but
/// "the same as the input" still means a `.ts` for a recording that came off
/// a disc, which is the shape the rest of this program is about. Asking for
/// M2TS on the output settings screen still gets one.
const TS_LIKE = ["m2ts", "mts", "m2t"];

/// The other family that comes out as a transport stream, for a different
/// reason.
///
/// A program stream cannot be written back as one that is worth having. A
/// DVD's own shape is not simply MPEG-PS: it is VOBUs of a bounded size, a
/// navigation pack opening each of them, and an `.IFO` beside the stream
/// describing every cell in it -- and a cut whose stream no longer matches
/// the index beside it is a disc that will not play. Writing a plain program
/// stream instead would be writing a file that is neither a DVD nor the shape
/// the rest of this program is about. So a cut of one is a transport stream,
/// carrying the same pictures and the same sound. Asking for something else
/// on the output settings screen still gets it.
const PS_LIKE = ["vob", "mpg", "mpeg", "m2p"];

/// A DVD title's name carries the sectors it plays -- `VTS_01_1.VOB@0-2081904`
/// -- and what it is written in is the part in front of that.
const streamOf = (p) => p.replace(/@\d+-\d+$/, "");

function containerFor(clip) {
  // A disc's recordings are `.m2ts` whatever the recording arrived as, so
  // that is what the audio has to be writable into -- which is not the same
  // question as what an `.mp4` can hold.
  if (bdavMode()) return "m2ts";
  if (settings.container) return settings.container;
  const ext = extOf(streamOf(clip.path));
  return TS_LIKE.includes(ext) || PS_LIKE.includes(ext) ? "ts" : ext || "mp4";
}

/// Where a clip would be written if it were the only one in the list.
function outputBase(clip) {
  const ext = containerFor(clip);
  // A recording on a disc has nowhere beside it to be written -- inside an
  // image there is no folder at all -- so the disc says where instead, and
  // what to call it: the programme's own name, not `00001`.
  const beside = clip.home ? `${clip.home.replace(/[/\\]*$/, "")}/` : dirOf(clip.path);
  // The folder of its own goes under whichever of the two was chosen -- the
  // one that was typed, or the one the recording came out of. A batch left
  // to write beside its inputs is exactly the case that wants it.
  const dir = `${beneath(outDir() || beside)}/`;
  return { dir, name: `${settings.prefix}${seqNo(clip)}${outStem(clip)}`, ext };
}

/// Which of the rows that would be written to the same file this one is,
/// counting from one, or "" when no other row wants that name.
///
/// Two copies of one recording are the obvious case, and the one the list
/// itself numbers. They are not the only case: two recordings of the same
/// programme from different folders share a name, and every recording on a
/// disc is called `00001`. Left alone, the second of them would be written
/// over the first without a word -- the run would report two files written
/// and one would be gone.
function outNo(clip) {
  const mine = outputBase(clip);
  const twins = clips.filter((c) => {
    const it = outputBase(c);
    return it.dir === mine.dir && it.name === mine.name && it.ext === mine.ext;
  });
  return twins.length > 1 ? String(twins.indexOf(clip) + 1) : "";
}

function outputPath(clip) {
  const { dir, name, ext } = outputBase(clip);
  const n = outNo(clip);
  return `${dir}${name}${n ? `_${n}` : ""}.${ext}`;
}

/// Where a run that writes the whole list as one file puts it.
///
/// The first row's own name, numbering and all -- which is what the list is
/// called on screen and what somebody would go looking for. Not a name of
/// its own: a joined file is the list, and the list already has a name at
/// the top of it. The subfolder still does its work, so twelve episodes
/// joined into one land in the folder the twelve would have.
function joinedPath() {
  const list = ready();
  if (!list.length) return "";
  const { dir, name, ext } = outputBase(list[0]);
  return `${dir}${name}.${ext}`;
}

/// Which control stands for which setting. Kept because the flow is
/// otherwise one-way -- the screen is where the settings are made, and the
/// only thing that ever makes them from the other side is a project opening.
const settingInputs = [];

function bindSetting(id, key, kind = "value") {
  const input = el(id);
  const read = () => {
    settings[key] = kind === "checked" ? input.checked : input.value;
    // Which is the whole of what settles the output: a control on this
    // screen used by a hand. See `outputSettled`.
    settleOutput();
    renderOutset();
    renderOutScreen();
    rememberOutput();
    touch();
  };
  input.addEventListener(kind === "checked" ? "change" : "input", read);
  settingInputs.push([input, key, kind]);
  if (kind === "checked") input.checked = settings[key];
  else input.value = settings[key] ?? "";
}

/// Put `settings` back on screen, for when something other than the screen
/// has changed them.
function showSettings() {
  for (const [input, key, kind] of settingInputs) {
    // A setting nobody has settled yet is left alone. It has no value to
    // show and none to read back: the screen decides one when it draws, and
    // reading the empty control here would settle it as "none" instead.
    if (settings[key] === null) {
      input.value = "";
      continue;
    }
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
  rememberOutput();
}
bindSetting("out-dir", "dir");
bindSetting("out-subfolder", "subfolder");
bindSetting("out-disc-title", "discTitle");
bindSetting("out-disc", "disc");
bindSetting("out-fit", "fit", "checked");
bindSetting("out-image", "image");
bindSetting("out-image-access", "imageAccess");
bindSetting("out-image-only", "imageOnly", "checked");
bindSetting("out-prefix", "prefix");
bindSetting("out-number", "number", "checked");
bindSetting("out-digits", "digits");
bindSetting("out-container", "container");
bindSetting("out-audio", "audio");
bindSetting("out-audio-codec", "audioCodec");
bindSetting("out-audio-channels", "audioChannels");
bindSetting("out-audio-bitrate", "audioBitrate");
bindSetting("out-audio-rate", "audioRate");
bindSetting("out-audio-bits", "audioBits");
bindSetting("out-subtitles", "subtitles");
bindSetting("out-keyframes", "keyframes", "checked");
bindSetting("out-join", "joinAll", "checked");

// --- drop-downs that open upward -----------------------------------------
//
// Drawn here rather than by the platform, for both of this window's reasons:
// where a native popup opens is the platform's to decide -- the bitrate
// ladder at the foot of the file settings ran off the edge -- and what it is
// drawn in is the system's own light colours. The machinery is shared with
// the seam window, which has lists of its own. See `wireDrops`.
wireDrops();

/// Whether the audio is being rebuilt rather than carried through.
///
/// The controls under the mode -- codec, channels, rate, width, bitrate --
/// only describe an encode, and the other two modes do not run one over the
/// whole track:
/// `copy` runs none at all, and `smart` runs one on two frames per boundary,
/// where the whole point is that they come out the same shape as the frames
/// they are spliced between. So they answer to the mode.
function reencodingAudio() {
  return settings.audio === "reencode";
}

/// Whether what is being written has no bitrate to choose.
///
/// Linear PCM is not an encode but a transcription: its size is arithmetic
/// -- channels times bits times the sample rate -- and nothing about it is a
/// rate anyone can pick. The engine ignores the figure; the control says so
/// by going grey and showing the arithmetic instead.
///
/// The mode is part of the question. A codec left set to リニア PCM while
/// the mode is smart rendering is a setting the cut never reaches, and an
/// uncompressed size shown against a track that is being copied through
/// would be a number about a file nobody is writing.
function codecHasNoBitrate() {
  return reencodingAudio() && settings.audioCodec === "lpcm";
}

/// Put on screen what the chosen mode has a use for, and grey the rest.
///
/// The five rows under the mode all describe an encode, and two of the three
/// modes do not run one: kept on screen they would be five grey rows to read
/// past every time the panel is opened, so they are absent until すべて再エン
/// コード asks for them. They keep their values while away -- the settings
/// are the same fields either way, and nothing reads them into a cut that is
/// not re-encoding.
///
/// Inside the encode the last pair stays a matter of grey rather than
/// absence. Both rows belong to the encode being written and which of the two
/// is live moves with the codec, so hiding them would shuffle the panel under
/// the hand of whoever is choosing a codec.
function lockAudioDetail() {
  for (const row of document.querySelectorAll(".audio-detail")) {
    row.hidden = !reencodingAudio();
  }
  // The mirror image of the bitrate: a width belongs to the codec that
  // writes samples down, a bitrate to the codecs that describe them, and
  // each control is grey exactly where the other is not.
  el("out-audio-bits").disabled = !codecHasNoBitrate();
  el("out-audio-bitrate").disabled = codecHasNoBitrate();
}

/// The rates each codec is spoken in, and how high its ladder goes at a
/// given channel count.
///
/// The rungs are the rates the format itself has: AAC's are conventional and
/// the encoder will take anything, but AC-3 and DTS both carry the rate as a
/// number in a table, and a figure between two of them is one the encoder
/// rounds to whichever it pleases. Offering the table is offering what will
/// actually be written.
///
/// The ceilings are what the rate is worth at that many channels. 384 kbit/s
/// for stereo AAC and 640 for 5.1 is where a broadcast puts them with room
/// over the top; AC-3 is carried at 192 for stereo and 448 for 5.1 on a
/// disc, with 640 its own limit; DTS has two rates anyone uses, 768 and
/// 1536, and the ladder is the way between them.
///
/// Worth knowing, though it is not what sets these: an encoder has a ceiling
/// of its own and does not announce it. Asked for more than it can spend,
/// FFmpeg's AAC encoder writes less -- driven with noise at 48 kHz so that it
/// and not the material runs out first, mono walls near 218 kbit/s and stereo
/// near 250. So the top of the stereo ladder is headroom rather than a
/// promise: ask for 384 of stereo and what comes back is what the encoder
/// found worth spending.
///
/// 入力と同じ is not in the table: what the recording carries is not known
/// until each clip is read, and for a broadcast it is AAC, so AAC's ladder
/// is the one to show.
const AUDIO_LADDERS = {
  aac: {
    rungs: [64, 80, 96, 112, 128, 144, 160, 192, 224, 256, 320, 384, 448, 512, 640],
    ceiling: { 1: 192_000, 2: 384_000, 6: 640_000 },
  },
  ac3: {
    rungs: [64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640],
    ceiling: { 1: 192_000, 2: 384_000, 6: 640_000 },
  },
  dts: {
    rungs: [384, 512, 768, 960, 1024, 1152, 1280, 1408, 1536],
    ceiling: { 1: 768_000, 2: 1_536_000, 6: 1_536_000 },
  },
};
for (const l of Object.values(AUDIO_LADDERS)) {
  l.rungs = l.rungs.map((k) => k * 1000);
  l.max = l.rungs[l.rungs.length - 1];
}

/// The ladder the chosen codec is spoken in. LPCM has none: what it costs is
/// arithmetic rather than a choice, and `lpcmBitRate` does the sum.
function ladder() {
  return AUDIO_LADDERS[settings.audioCodec] || AUDIO_LADDERS.aac;
}

/// Every sound track a clip's cut will carry.
///
/// Which tracks are kept is answered in stream indices once the editor has
/// been in (`edit.dropStreams`) and in PIDs until then (`dropPids`) -- the
/// same two answers the export sends, resolved here the same way round.
///
/// Empty for a clip with no sound, and for one whose every sound track was
/// switched off: both come out of the cut the same way.
function keptAudio(clip) {
  // The container names the sound tracks, so this is answerable from the
  // cheap first look and does not wait for the walk. It matters: the
  // properties panel would otherwise say 音声: なし about a recording that
  // has sound, for as long as the walk in front of it takes.
  const i = factsOf(clip);
  if (!i || !i.has_audio) return [];
  const tracks = i.audio_tracks || [];
  // Read by a version that did not list the tracks: the main track is all
  // there is to go on, and it is the right answer wherever nothing was
  // switched off -- which is every recording that carries one track. Its
  // codec was not sent either, and an unnamed track is one nothing can be
  // decided about -- see `writableSound`.
  if (!tracks.length) {
    return [
      {
        codec: "",
        channels: i.audio_channels || 0,
        sample_rate: i.audio_sample_rate || 0,
        bits: i.audio_bits || 0,
      },
    ];
  }
  const dropped = clip.edit ? clip.edit.dropStreams || [] : null;
  return dropped
    ? tracks.filter((a) => !dropped.includes(a.index))
    : tracks.filter((a) => !(clip.dropPids || []).includes(a.pid));
}

/// The sound track a clip's figures are to be taken from: the first one the
/// cut will keep.
///
/// Not the main track. libavformat calls the widest track the main one, and
/// on a pressed disc that is the English 5.1 sitting beside the Japanese
/// stereo -- so a clip that keeps only the second would still be shown as
/// 5.1, offered a downmix from it, and costed at six channels. The first
/// *kept* track is the one the output opens with, and that is what the one
/// figure these lines have room for should speak for.
///
/// Null for a clip with no sound, and for one whose every sound track was
/// switched off.
function audioOf(clip) {
  return keptAudio(clip)[0] || null;
}

/// What one clip's sound will cost per second written as linear PCM.
///
/// Channels times bit depth times the sample rate, and the only thing to
/// know beyond that is where each of the three comes from. The channels are
/// the output's, so a fold halves the figure. The depth is the recording's
/// -- 24 bits only where it has more than 16 in it, which off a broadcast it
/// never does -- and the engine settles it in `pcm_bits`.
///
/// The wrinkle is the container. Blu-ray's LPCM is the only shape a
/// transport stream can carry, and it writes its channels in pairs: an odd
/// count is padded with a silent one, so a mono track in a `.ts` costs two
/// channels' worth of bytes. Everywhere else the count is the count.
///
/// 0 for a clip with no sound, or one read by a version that did not say.
function lpcmBitRate(clip, container) {
  const sound = audioOf(clip);
  if (!sound) return 0;
  const channels = audioChannelsOut() || sound.channels || 0;
  const bits = audioBitsOut() || sound.bits || 0;
  const rate = writableRate(audioRateOut() || sound.sample_rate || 0, container);
  if (!channels || !bits || !rate) return 0;
  const paid = isTsContainer(container) ? channels + (channels % 2) : channels;
  return paid * bits * rate;
}

function isTsContainer(container) {
  return container === "ts" || TS_LIKE.includes(container);
}

/// The rates Blu-ray LPCM -- the only linear PCM a transport stream can
/// declare -- is written at. Nothing between them, and nothing below 48.
const BLURAY_LPCM_RATES = [48000, 96000, 192000];

/// The rate a track will actually be written at, given the rate asked for
/// and where it is going.
///
/// Not every codec speaks every rate, and of the four this window offers one
/// codec falls short of the list: Blu-ray's LPCM has 48, 96 and 192 kHz and
/// nothing between, so 44.1 and 32 asked of a `.ts` come back as 48, while 96
/// -- which reaches here off a disc that was recorded at it, since that is
/// what the ceiling lets be chosen -- is written as asked, and so is a 96 kHz
/// recording carried through as 入力と同じ. AAC speaks all four; AC-3 and DTS
/// have neither 96 nor anything above 48, and there the window is told so by
/// the engine and greys the rate out rather than moving it. The plain
/// big-endian PCM every other container gets lists no rates at all and takes
/// anything.
///
/// The engine settles this in `audio::writable_rate` and the window has to
/// arrive at the same answer, because what this decides is the figure shown
/// in place of an uncompressed track's bitrate -- and a figure about a file
/// nobody is writing would be worse than no figure at all. The nearest rate
/// the encoder lists, and the higher of two equally near, which is the same
/// tie the engine breaks in `encoder_rate`.
function writableRate(hz, container) {
  if (!hz || settings.audioCodec !== "lpcm" || !isTsContainer(container)) return hz;
  return BLURAY_LPCM_RATES.reduce((best, r) => {
    const near = Math.abs(r - hz) - Math.abs(best - hz);
    return near < 0 || (near === 0 && r > best) ? r : best;
  });
}

/// What to write in the bitrate control for linear PCM, which is told rather
/// than asked.
///
/// The setting is one answer for a whole list, and the clips in it need not
/// cost the same -- a stereo recording beside a 5.1 one, read as 入力と同じ,
/// is two figures. Naming a range says that honestly; naming one of them
/// would be picking a clip at random and calling it the answer.
function lpcmLabel() {
  const rates = ready()
    .map((c) => lpcmBitRate(c, containerFor(c)))
    .filter((b) => b > 0);
  const distinct = [...new Set(rates)].sort((a, b) => a - b);
  if (!distinct.length) return t("bitrate.none");
  const kbps = (b) => b / 1000;
  return distinct.length === 1
    ? t("bitrate.fixed", { rate: kbps(distinct[0]) })
    : t("bitrate.fixedRange", { from: kbps(distinct[0]), to: kbps(distinct[distinct.length - 1]) });
}

/// The ceiling for any channel count, named or not.
///
/// The counts the control offers are named. A recording read as 入力と同じ
/// can be any count at all, and one of those takes the ceiling of the next
/// count up -- a 4-channel recording is nearer 5.1 than it is stereo.
function bitrateCap(channels) {
  const l = ladder();
  if (!channels) return l.max;
  const key = Object.keys(l.ceiling)
    .map(Number)
    .find((n) => n >= channels);
  return key ? l.ceiling[key] : l.max;
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
  const counts = ready().map((c) => {
    const sound = audioOf(c);
    return (sound && sound.channels) || 0;
  });
  return counts.length ? Math.max(...counts) : 0;
}

// --- what may actually be written ----------------------------------------
//
// The five controls above offer what these formats are ordinarily asked for,
// and not every one of those can be written. Blu-ray LPCM -- the only linear
// PCM a transport stream can declare -- has 48, 96 and 192 kHz and nothing
// between, so 44.1 kHz asked of a `.ts` is a rate that will not come out.
// DTS is written mono, stereo, quad, 5.0 or 5.1 and in no other count, and
// its frame has to be long enough to describe every channel in it, which
// puts a floor under the bitrate that moves with the channels and with the
// rate: 5.1 at 48 kHz is not written under about 670 kbit/s, and 384 kbit/s
// asked of it is a cut that stops where the encoder is opened.
//
// So the window offers what can be written and greys the rest. What can be
// is not a table kept here: the answers are libav's encoders' own, the
// encoders are whatever FFmpeg the build was linked against, and a table
// here would go quietly out of date. The engine is asked instead, and
// answers by opening encoders and seeing.

/// Every sound track this list will write, as the engine wants it described.
///
/// Every track and not the first: the cut writes them all, each through an
/// encoder of its own, and a screen that answered for the first would offer
/// a codec the second cannot be written in.
function writtenTracks() {
  return ready().flatMap((c) =>
    keptAudio(c).map((a) => ({
      codec: a.codec || "",
      channels: a.channels || 0,
      rate: a.sample_rate || 0,
      bits: a.bits || 0,
      ts: isTsContainer(containerFor(c)),
    })),
  );
}

/// The recording's own codec, in the engine's spelling of it: an empty
/// control is the absence of a choice here and `source` there.
const asCodec = (v) => v || "source";

/// The lists being asked about, which are the controls' own.
///
/// Read off the controls rather than written out again, so that a codec
/// added to the window is a codec asked about. The bitrate control is the
/// exception: its options are built from the answer to this, so what is
/// asked about there is the ladder the answer will be drawn from.
function audioOffer() {
  const values = (id) => [...el(id).options].map((o) => o.value);
  return {
    // 入力と同じ is an empty control and a named codec in the engine.
    codecs: values("out-audio-codec").map(asCodec),
    channels: values("out-audio-channels").map(Number),
    rates: values("out-audio-rate").map(Number),
    bits: values("out-audio-bits").map(Number),
    bitrates: ladder().rungs.filter((b) => b <= bitrateCap(channelsForCap())),
  };
}

/// What the engine has said can be written, kept by the question it answers.
///
/// The answer takes a few dozen encoder opens, which is why it is asked for
/// off the UI thread and why it is kept: the panel is redrawn on every
/// keystroke in the prefix field, and none of those change the answer. Null
/// while there is no answer yet -- the first draw after a setting changes
/// offers everything, and the answer, when it lands, draws the panel again.
const soundLimits = new Map();
const soundAsked = new Set();

function writableSound() {
  // The controls are only on screen while the whole track is being rebuilt,
  // and only then does anything reach an encoder. There is nothing to refuse
  // in a mode that copies frames.
  if (!reencodingAudio()) return null;
  const tracks = writtenTracks();
  // A list with nothing in it yet, or nothing with sound in it: no track to
  // answer for, and no answer worth greying a control on.
  if (!tracks.length) return null;
  const held = {
    codec: asCodec(settings.audioCodec),
    channels: Number(settings.audioChannels) || 0,
    rate: Number(settings.audioRate) || 0,
    bits: Number(settings.audioBits) || 0,
    bitrate: Number(settings.audioBitrate) || 0,
  };
  const offer = audioOffer();
  const ask = { tracks, held, offer };
  const key = JSON.stringify(ask);
  if (soundLimits.has(key)) return soundLimits.get(key);
  if (invoke && !soundAsked.has(key)) {
    soundAsked.add(key);
    invoke("audio_limits", ask)
      .then((can) => {
        soundLimits.set(key, can);
        soundAsked.delete(key);
        renderOutset();
      })
      .catch((e) => {
        soundAsked.delete(key);
        jlog(`audio_limits: ${e}`);
      });
  }
  return null;
}

/// Where a control goes when what it was holding cannot be written.
///
/// 入力と同じ, which is not an answer but the absence of one, and so is
/// always somewhere to fall back to. The rate is the exception: a rate a
/// codec cannot speak is one the engine would write at the nearest it can,
/// so the control is put where the file would have gone rather than made to
/// forget the question was asked.
///
/// Except when the rate is over the ceiling, which is the other reason it
/// can be refused. A rate above what the recording was sampled at is not one
/// the engine would move -- it would write it, and that is the whole point
/// of refusing it -- so there is no place the file was going to fall to.
/// Nearest would be a resample of every other clip in the list, chosen by
/// nobody, so it goes to 入力と同じ with the other two.
function insteadOf(key, was, allowed, ceiling) {
  if (key === "audioRate" && was && !(ceiling && Number(was) > ceiling)) {
    const want = Number(was);
    const near = allowed
      .filter((v) => v)
      .map(Number)
      .sort((a, b) => Math.abs(a - want) - Math.abs(b - want) || b - a)[0];
    if (near) return String(near);
  }
  return allowed.includes("") ? "" : allowed[0] || "";
}

/// The most each of the three sample settings may be offered at: what the
/// recording already has in it.
///
/// A re-encode can write more channels than were sent, a faster grid than
/// was sampled, or wider samples than were recorded, and each of those is a
/// bigger file holding exactly the sound that went in. An upmix invents the
/// channels it spreads into; a resample upwards draws the same curve through
/// more points; a 16 bit recording written 24 bits wide pads every sample
/// with zeroes. None of the three is a thing to offer someone shortening a
/// broadcast, so these controls offer the way down and `入力と同じ`, and
/// nothing above what came in.
///
/// 96 kHz is on the rate's list for the recordings that have it -- a Blu-ray
/// carrying LPCM or lossless sound at 96 -- and is greyed out under a
/// broadcast for the same reason 24 bit is: 48 kHz sampled and written at 96
/// is the same curve drawn through more points.
///
/// The least of the tracks rather than the most. The setting is one answer
/// for the whole list and every track in it is written through it, so an
/// answer above the narrowest track's own would raise that one -- and a
/// pressed disc that carries English 5.1 beside Japanese stereo is a single
/// recording with both in it. A zero is a track that never said, which
/// decides nothing, and a list with nothing readable in it yet decides
/// nothing either: there the whole list is offered, as it was before there
/// was anything to ask.
function soundCeiling() {
  // The mode is part of the question, as it is everywhere else on this
  // panel: the rows are off the screen while frames are being copied, and
  // nothing reads what they hold into a cut that is not re-encoding.
  if (!reencodingAudio()) return { channels: 0, rate: 0, bits: 0 };
  const tracks = writtenTracks();
  const least = (of) => {
    const said = tracks.map(of).filter((v) => v > 0);
    return said.length ? Math.min(...said) : 0;
  };
  return {
    channels: least((t) => t.channels),
    rate: least((t) => t.rate),
    bits: least((t) => t.bits),
  };
}

/// What a control is holding, unless that is more than the recording has.
///
/// `lockUnwritable` takes a control off such an answer as soon as the panel
/// is drawn, and it is drawn again whenever a clip finishes being read. This
/// is the same arithmetic done where the answer is used, so that a count
/// chosen against a 5.1 recording cannot reach the cut of a stereo one that
/// joined the list after it. A ceiling of zero is a list that decides
/// nothing, and there what is held stands.
function under(want, ceiling) {
  if (!want) return null;
  return ceiling && want > ceiling ? null : want;
}

/// Which containers the engine has said this list can be written into, kept
/// by the question it answers.
///
/// The engine is asked rather than a table kept here, exactly as the audio
/// controls ask it: what a container holds belongs to the muxers this build
/// was linked against, and a copy of that kept on the screen would drift
/// from them. See `carry` in the engine.
///
/// Null while there is no answer yet, and null for a list nothing has been
/// read out of. Everything is offered then: a control greyed before there is
/// a recording to grey it for says the program cannot write something when
/// all it means is that it has not been asked.
const containerCan = new Map();
const containerAsked = new Set();

function writableContainers() {
  const list = ready();
  const facts = list.map(factsOf);
  if (!list.length || facts.some((i) => !i || !i.codec)) return null;
  const once = (names) => [...new Set(names.filter(Boolean))];
  const lower = (s) => String(s || "").toLowerCase();
  const ask = {
    // Read off the control rather than written out again, so a container
    // added to the window is a container asked about.
    want: [...el("out-container").options].map((o) => o.value).filter(Boolean),
    video: once(facts.map((i) => lower(i.codec))),
    audio: once(list.flatMap((c) => keptAudio(c).map((a) => lower(a.codec)))),
    // What the sound will be written as, which is what the cut is sent.
    asked: audioCodecOut() || "",
  };
  const key = JSON.stringify(ask);
  if (containerCan.has(key)) return containerCan.get(key);
  if (invoke && !containerAsked.has(key)) {
    containerAsked.add(key);
    invoke("containers_holding", ask)
      .then((can) => {
        containerCan.set(key, can);
        containerAsked.delete(key);
        renderOutset();
      })
      .catch((e) => {
        containerAsked.delete(key);
        jlog(`containers_holding: ${e}`);
      });
  }
  return null;
}

/// Grey the containers this list cannot be written into.
///
/// There used to be one answer here and it was WebM, on the grounds that
/// every other container takes whatever reaches it. That stopped being true
/// when VP8, VP9 and AV1 were taken as input: a transport stream has no
/// stream type for any of the three, declares them as private data of no
/// stated kind, and writes the file without complaint -- and every player
/// reads it back as no pictures at all. QuickTime turns the three away
/// outright, and lossless sound with them. So each container is asked about
/// now, and about the sound as well as the pictures.
///
/// 入力と同じ is never greyed. It is the absence of a choice rather than a
/// container: each recording goes back into the kind it came out of, which
/// held those codecs already.
function lockContainer() {
  const can = writableContainers();
  for (const opt of el("out-container").options) {
    opt.disabled = !!can && !!opt.value && !can.includes(opt.value);
  }
  if (!can || !settings.container || can.includes(settings.container)) return;
  // Held and no longer writable: back to 入力と同じ, which is always
  // somewhere to fall back to.
  settings.container = "";
  el("out-container").value = "";
}

/// Grey out every answer that cannot be written or would be more than the
/// recording has, and take a control off one it is already holding.
///
/// Greyed rather than dropped from the list. Which answers are missing is
/// the one thing a shortened list cannot say, and a codec that is absent
/// because of the recording in the list looks like a codec this program does
/// not have.
function lockUnwritable() {
  const can = writableSound();
  const cap = soundCeiling();
  // A zero and a `source` are the engine's way of saying the recording's
  // own, which is the empty option at the top of every one of these lists.
  const named = (v) => (!v || v === "source" ? "" : String(v));
  for (const [id, key, told, ceiling] of [
    ["out-audio-codec", "audioCodec", can && can.codecs.map(named), 0],
    ["out-audio-channels", "audioChannels", can && can.channels.map(named), cap.channels],
    ["out-audio-rate", "audioRate", can && can.rates.map(named), cap.rate],
    ["out-audio-bits", "audioBits", can && can.bits.map(named), cap.bits],
  ]) {
    const select = el(id);
    // Two questions asked of the same list, and an answer has to pass both:
    // whether an encoder will write it, which is null until the engine has
    // said, and whether it is more than the recording has, which is
    // arithmetic and needs nobody asked. 入力と同じ passes every ceiling --
    // it is the recording's own figure by definition.
    const offered = [...select.options].map((o) => o.value);
    const allowed =
      told || ceiling
        ? offered.filter((v) => (!told || told.includes(v)) && (!ceiling || Number(v) <= ceiling))
        : null;
    // No answer, or none yet: everything is on offer again rather than left
    // grey on the strength of a question that is no longer being asked.
    for (const opt of select.options) opt.disabled = !!allowed && !allowed.includes(opt.value);
    if (!allowed || allowed.includes(settings[key])) continue;
    settings[key] = insteadOf(key, settings[key], allowed, ceiling);
    select.value = settings[key];
  }
}

/// Put the rungs worth offering in the bitrate control, and bring the answer
/// it is holding inside them.
function fillBitrates() {
  const select = el("out-audio-bitrate");
  // A codec with no rate to choose gets a dash and nothing else, and the
  // setting goes with it: a figure left standing behind a greyed-out control
  // would come back the moment the codec changed, as an answer nobody gave.
  const none = codecHasNoBitrate();
  const cap = bitrateCap(channelsForCap());
  // Under the ceiling, and above whatever floor the codec has at this many
  // channels and this rate: DTS has one and it moves with both, so which
  // rungs are worth offering is the engine's answer rather than the
  // ladder's. No answer yet means the ladder, which is what was offered
  // before there was anything to ask.
  const can = writableSound();
  const rungs = none
    ? []
    : ladder().rungs.filter((b) => b <= cap && (!can || can.bitrates.includes(b)));
  if (none) settings.audioBitrate = "";
  // What a greyed-out LPCM control says: not a dash, which would leave the
  // one question it is there to answer unanswered, but the figure the file
  // will actually be written at.
  const told = none ? lpcmLabel() : "";
  // Rebuilt only when it would come out different -- which the language is
  // part of, since おまかせ is a word and not a number, the codec is, since
  // two of them count in different numbers, and the told figure is, since it
  // moves with the clips in the list and the channels asked of them.
  const sig = `${currentLang()}|${settings.audioCodec}|${rungs.join(",")}|${told}`;
  if (select.dataset.sig !== sig) {
    select.dataset.sig = sig;
    select.innerHTML = none
      ? `<option value="">${esc(told)}</option>`
      : `<option value="" data-i18n="bitrate.auto">${esc(t("bitrate.auto"))}</option>` +
        rungs.map((b) => `<option value="${b}">${b / 1000} kbps</option>`).join("");
  }
  // A rate the list no longer offers -- the channel count came down under it,
  // the codec changed to one that counts in other numbers, or a project was
  // written by a version whose ladder had other rungs -- is taken to the
  // nearest rung at or below it rather than thrown away.
  const want = Number(settings.audioBitrate) || 0;
  if (want && !rungs.includes(want)) {
    settings.audioBitrate = String(rungs.filter((b) => b <= want).pop() ?? rungs[0] ?? "");
  }
  select.value = settings.audioBitrate;
}

/// What the engine will actually be asked for. A control that is greyed out
/// still holds whatever it was last set to -- that is the point of greying it
/// out rather than clearing it -- and what it holds must not reach the cut
/// behind the screen's back.
function audioChannelsOut() {
  if (!reencodingAudio()) return null;
  return under(Number(settings.audioChannels), soundCeiling().channels);
}

function audioBitrateOut() {
  if (!reencodingAudio() || codecHasNoBitrate()) return null;
  return settings.audioBitrate ? Number(settings.audioBitrate) : null;
}

function audioRateOut() {
  if (!reencodingAudio()) return null;
  return under(Number(settings.audioRate), soundCeiling().rate);
}

/// Only where samples are what is being written. Everywhere else the control
/// is grey, and what a grey control is holding must not reach the cut behind
/// the screen's back.
function audioBitsOut() {
  if (!codecHasNoBitrate()) return null;
  return under(Number(settings.audioBits), soundCeiling().bits);
}

function audioCodecOut() {
  return reencodingAudio() && settings.audioCodec ? settings.audioCodec : null;
}

/// What a chosen codec is called on screen. Empty for 入力と同じ, which is
/// not a codec but the absence of a choice.
function codecLabel() {
  const want = audioCodecOut();
  if (!want) return "";
  // The list's own names, except that リニア PCM（非圧縮） is a label for a
  // menu and too long for a line that also has to hold the channels and the
  // rate.
  return want === "lpcm" ? t("codec.lpcm.short") : t(`codec.${want}`);
}

/// What is happening to the audio, when it is not being copied.
///
/// The notes on the output screen are about pictures -- that is what it shows
/// -- and "the whole clip is copied losslessly" stops being true of the file
/// the moment the audio is re-encoded from end to end, which a downmix always
/// is. So the picture's own claim carries this after it.
///
/// `fit` is what a join has settled about this row -- a track that does not
/// match the master's is written afresh however the settings are set, because
/// a track is declared once and what the stream says has to describe every
/// frame on it. Said only where the settings have not already said it.
function audioNote(clip, fit = null) {
  const sound = audioOf(clip);
  if (!sound) return "";
  if (!reencodingAudio()) {
    return fit && fit.audio ? " " + t("out.audioConformed") : "";
  }
  const from = sound.channels || 0;
  const to = audioChannelsOut() || from;
  if (from && to && to !== from) {
    // Which way it goes is the recording's to decide, not the setting's: one
    // list can hold a 5.1 recording and a stereo one, and 2ch asked of both
    // folds the first and spreads the second.
    const key = to < from ? "out.audioDownmixed" : "out.audioUpmixed";
    return " " + t(key, { from: chLabel(from), to: chLabel(to) });
  }
  const codec = codecLabel();
  return " " + (codec ? t("out.audioAsCodec", { codec }) : t("out.audioReencoded"));
}

/// A sample rate as it is spoken: 48 kHz, 44.1 kHz.
function khzLabel(hz) {
  return `${hz % 1000 ? (hz / 1000).toFixed(1) : hz / 1000} kHz`;
}

/// What the output's audio will be, in the one line the format panel has.
function audioSummary(clip, container) {
  const sound = audioOf(clip);
  if (!sound) return t("media.audioNo");
  const from = sound.channels || 0;
  const to = audioChannelsOut() || from;
  const down = !!(from && to && to !== from);
  // Here the figure can be exact, because here there is one clip: the
  // control above has a whole list to answer for and may only be able to
  // name a range.
  const rate = codecHasNoBitrate() ? lpcmBitRate(clip, container) : audioBitrateOut();
  const detail = [];
  const codec = codecLabel();
  if (codec) detail.push(codec);
  if (from) detail.push(down ? `${chLabel(from)} → ${chLabel(to)}` : chLabel(from));
  // The rate only when it is changing. The line has room for what the
  // settings are doing to the sound, not for restating what the recording
  // already was -- the format panel above says that.
  const hzFrom = sound.sample_rate || 0;
  const hzTo = writableRate(audioRateOut() || hzFrom, container);
  if (hzFrom && hzTo !== hzFrom) detail.push(`${khzLabel(hzFrom)} → ${khzLabel(hzTo)}`);
  // The width, on the other hand, only exists as a choice: it is grey unless
  // linear PCM is what is being written, and then it is the whole story of
  // what the track will cost.
  const bits = audioBitsOut();
  if (bits) detail.push(`${bits} bit`);
  if (rate) detail.push(`${rate / 1000} kbps`);
  const mode = t(`audio.${settings.audio}.short`);
  return detail.length ? t("outset.audioLine", { mode, detail: detail.join(", ") }) : mode;
}

// --- what a disc's index will say ----------------------------------------
//
// A file's name is the whole of what a cut of a recording is called: put
// `cut_2026年08月17日01時00分-BS11...ts` in a folder and everything about it
// is on the row. A disc is not like that. Its streams are called `00001.m2ts`
// and everything a person reads -- the programme, the night it went out, the
// name of the disc itself -- is in the index beside them, which is a place
// that has to be *filled in*.
//
// So the answer is gathered rather than derived. A recording read off a disc
// arrived with the name and the moment its own playlist carried; a broadcast
// recording carries them in its own tables and is asked. Either can be typed
// over, because a name nobody can correct is a name that is wrong forever.

/// Whether a disc is what this run will produce.
///
/// Written as a declaration rather than as a name bound to an arrow because
/// the screen is drawn once on the way past this point in the file, and a
/// name that is not bound yet is an error rather than a false.
function bdavMode() {
  return settings.mode === "bdav";
}

/// What the recording says about its own programme, asked once per clip.
///
/// Cheap -- it is a read of the first few megabytes -- but not free, and the
/// answer cannot change while the file does not. `{}` for a recording that
/// says nothing, so that "asked and it said nothing" is not asked again.
///
/// The ask itself is held, not just its answer: the screen and the disc's
/// own name both want this, and two callers arriving while the file is being
/// read should wait on the one read rather than start a second.
async function askProgramme(clip) {
  if (clip.said) return clip.said;
  clip.asking ??= readProgramme(clip);
  return clip.asking;
}

/// The read itself. Held on the clip while it is running and let go of when
/// the answer is on the clip instead.
async function readProgramme(clip) {
  try {
    clip.said = (await invoke("programme", { path: clip.path })) || {};
  } catch {
    clip.said = {};
  }
  clip.asking = null;
  return clip.said;
}

/// What a disc's index will call this recording.
///
/// In order: what somebody typed, what the disc it came off called it, what
/// the broadcast says it is, and -- for a recording that has been through
/// tools that kept none of that -- the name of the file, which is at least
/// something a person chose once.
function programmeOf(clip) {
  if (clip.programme) return clip.programme;
  // A row renamed in the list is a row somebody has named, which beats what
  // the recording or the disc it came off says about itself. The field on the
  // output screen still wins: that one is about the disc.
  if (clip.renamed) return clip.renamed;
  if (clip.stem) return clip.name;
  const said = clip.said || {};
  return said.name || stemOf(clip.path);
}

/// And when it says the recording was made. The disc's answer first: it is
/// the one a person has already seen in a list of recordings.
///
/// These three and the number below hold whatever will be written, which is
/// what the recording arrived with until somebody types over it -- so `null`
/// is "nobody has said" and falls through to what the recording says about
/// itself, while an empty string is an answer: leave the field on the disc
/// empty. Which is a thing a real disc does. The authoring tool's disc
/// writes the programme and the date and leaves the channel and the
/// description blank, because a file handed to it is not a broadcast.
///
/// The name is not like that -- see `programmeOf`, which fills an emptied
/// field back in. A nameless row in a recorder's list is the one outcome
/// nobody wants.
function madeOf(clip) {
  return clip.made ?? (clip.said || {}).made ?? null;
}

/// What the broadcaster said the programme was: the sentence a listing
/// carries, and the cast and staff under it. A recorder writes this into the
/// playlist beside the name, and shows it when the programme is selected.
function descriptionOf(clip) {
  return clip.description ?? (clip.said || {}).description ?? null;
}

/// The channel it came off, and the three digits a viewer knows that channel
/// by -- 0 for a terrestrial recording, whose three digits are in a table
/// this does not read. Field by field, so a disc that named the programme
/// and not the channel still takes the channel from the stream.
function channelOf(clip) {
  return clip.channel ?? (clip.said || {}).channel ?? null;
}

function channelNumberOf(clip) {
  return clip.channelNumber ?? (clip.said || {}).channel_number ?? 0;
}

/// How much room each of the index's texts has, in bytes of ARIB code.
///
/// The playlist gives the name a length byte and 255 bytes, the channel 20,
/// and the description everything between where it starts and where the play
/// items do. See `rpls` in `bdav.rs`, where these are the same four numbers.
const ROOM = { name: 255, channel: 20, about: 1200 };

/// What a text will cost in the field it is going into.
///
/// The index's texts are ARIB eight-unit code rather than UTF-8, and what
/// does not fit is cut off at the far end without anybody being told. So it
/// is counted here, where there is still somebody to tell.
///
/// The same arithmetic `arib::encode_within` does: an ASCII character is a
/// byte, anything else is a JIS pair, and the first character of a run costs
/// two bytes more for the shift into the set it is written against. A space
/// and a line break are in neither set and leave the state alone.
function aribBytes(text) {
  let n = 0;
  let mode = null;
  for (const c of text) {
    // `\` and `~` are the two cells JIS X 0201 spends on the money sign and
    // the overline, so they go through the wide set as themselves.
    const narrow =
      c === "¥" || c === "‾" || (c >= "!" && c <= "~" && c !== "\\" && c !== "~");
    const against = c === "\n" || c === " " ? null : narrow ? "alnum" : "kanji";
    if (against && against !== mode) {
      n += 2;
      mode = against;
    }
    n += against === "kanji" ? 2 : 1;
  }
  return n;
}

/// A moment as the playlist carries it: `2026-08-17 01:00:00`, or null for
/// anything that cannot be read as one.
///
/// The same bounds `si::Began::parse` holds to, because that is what will
/// read this back -- and a date it cannot read is written as no date at all.
/// What is looser here is only the typing: slashes for dashes, a `T` for the
/// space, a missing seconds field and single digits are all understood and
/// come back in the one shape the disc uses.
function madeParse(text) {
  const m = /^\s*(\d{4})[-/.](\d{1,2})[-/.](\d{1,2})[ T](\d{1,2}):(\d{1,2})(?::(\d{1,2}))?\s*$/.exec(
    text
  );
  if (!m) return null;
  const [y, mo, d, h, mi, se] = m.slice(1).map((v) => Number(v || 0));
  if (y < 1970 || y > 2200 || mo < 1 || mo > 12 || d < 1 || d > 31) return null;
  if (h > 23 || mi > 59 || se > 59) return null;
  const pad = (n, w = 2) => String(n).padStart(w, "0");
  return `${pad(y, 4)}-${pad(mo)}-${pad(d)} ${pad(h)}:${pad(mi)}:${pad(se)}`;
}

/// Where the chapter points of a recording written onto a disc go.
///
/// Every kept range begins one. That is where the cuts are, and skipping to
/// the far side of a commercial break is the whole of what a chapter point on
/// a recording is for. The marks made in the editor go in beside them --
/// somebody put those down deliberately -- and a mark that lands on a range
/// boundary is one chapter and not two.
function chaptersFor(clip) {
  const keeps = keepsOf(clip);
  const out = keeps.map((k) => k.at);
  for (const at of clip.edit ? clip.edit.keyframes : []) {
    const mapped = srcToOut(keeps, at);
    if (mapped !== null) out.push(mapped);
  }
  out.sort((a, b) => a - b);
  return out.filter((at, i) => i === 0 || at - out[i - 1] > 0.5);
}

/// A name made safe to be a folder's, the way the engine makes one.
///
/// The same replacements `disc::filename` makes and the same 180 byte cut:
/// this is a title somebody typed for a disc, and a title is free to hold a
/// slash or a colon where a path is not. Full width stand-ins rather than
/// removals, because a title that reads the same is worth more than a name
/// that is a few characters shorter.
const FORBIDDEN = { "\\": "＼", "/": "／", ":": "：", "*": "＊", "?": "？", '"': "＂", "<": "＜", ">": "＞", "|": "｜" };
function filenameSafe(name) {
  let out = "";
  let bytes = 0;
  for (const c of name) {
    const safe = FORBIDDEN[c] || (c < " " ? " " : c);
    const n = new TextEncoder().encode(safe).length;
    if (bytes + n > 180) break;
    out += safe;
    bytes += n;
  }
  return out.replace(/^[\s.\u3000]+|[\s.\u3000]+$/g, "");
}

/// Whether anything in the list is a DVD title.
///
/// Which is either kind of disc: a DVD draws its subtitles and so does a
/// Blu-ray, and either can go inside the cut or beside it. A broadcast's are
/// not this kind at all, and the question is not asked of one.
///
/// `"dvd"`, `"bdmv"`, `"both"` where the list holds some of each, or null
/// where nothing in it draws its subtitles. Which of the two it is decides
/// how the choice is worded: see `paintSubtitleChoices`.
///
/// Asked of the name rather than of the tracks, because a title's name
/// carries the sectors it plays and nothing else does -- and the tracks are
/// read when a clip is opened, which is later than this row has to be right.
/// A name survives a project being saved and opened again, which is the
/// other reason.
function drawnSubtitles() {
  let dvd = false;
  let bdmv = false;
  for (const c of clips) {
    const path = c.path || "";
    if (/\.vob@\d+-\d+$/i.test(path)) dvd = true;
    else if (/[\\/]BDMV[\\/]STREAM[\\/][^\\/]+$/i.test(path)) bdmv = true;
  }
  if (dvd && bdmv) return "both";
  return dvd ? "dvd" : bdmv ? "bdmv" : null;
}


/// Word the three destinations for the disc the list came off.
///
/// The three are the same either way and what they mean is not. A DVD's
/// subtitles travel **untouched** in the pair beside the cut and are
/// converted into either of the other two; a Blu-ray's are untouched inside
/// the cut and in the `.sup` beside it, and converted into the pair. Which
/// one leaves them alone is the whole of what a person is choosing between,
/// so it is what the line says -- and it is a different line for each disc.
///
/// A list holding some of each gets the neutral wording, which is true of
/// both and says less. The default is the same one throughout: inside the
/// cut, whichever disc it came off.
///
/// Written as `data-i18n` and not only as text, so that the language picker
/// finds the right string here as it does everywhere else.
function paintSubtitleChoices(from) {
  const suffix = from === "both" ? "" : `.${from}`;
  const say = (node, key) => {
    if (!node) return;
    node.dataset.i18n = key;
    node.textContent = t(key);
  };
  say(el("label-subtitles"), `outset.subtitles${suffix}`);
  for (const option of el("out-subtitles").options) {
    say(option, `subtitles.${option.value}${suffix}`);
  }
}

/// Whether this run writes into a folder of its own.
///
/// Always for a disc, which is a dozen files with names it chose itself and
/// belongs nowhere near anything else. For files, only where there is more
/// than one of them: a single cut goes where it was told to go, and burying
/// it one level down is one more folder to open for no reason.
function subfolderWanted() {
  return bdavMode() || ready().length > 1;
}

/// What that folder is called when nobody has said.
///
/// The disc's own name where there is a disc, and the project's where there
/// is a project. An evening's work saved as `2026-09-08.scproj` names the
/// folder after itself; an unsaved list has only today to go on, which is
/// still better than the cuts landing loose in the folder above. Today by
/// the clock in the room -- see `today`, and the hour this kind of work is
/// done at.
function autoSubfolder(list) {
  const name = bdavMode()
    ? discTitleFor(list)
    : projectPath
      ? stemOf(projectPath)
      : today();
  return filenameSafe(name);
}

/// Where the run in progress is writing, or `null` between runs: the folder
/// it was told to write into, and the folder of its own it makes under it.
///
/// Both are frozen for the length of a run. The settings screen stays open
/// and editable while the work goes on, and a path typed into it half way
/// through would otherwise move the output out from under the recordings
/// already written -- fatal for a disc, whose streams would then be in one
/// folder and its index in another.
let runDir = null;
let runFolder = null;

/// The folder the output goes in, as this run sees it.
function outDir() {
  return runDir ?? settings.dir;
}

/// The name the folder this run makes is asked to have, before anything is
/// done about one of that name already being there.
///
/// The one on the screen where somebody has settled one, and otherwise the
/// one the screen *would* fill in. Worked out here rather than read out of
/// the setting, because a run can be started from the output screen without
/// the settings screen ever having been drawn, and a folder that appears
/// only for people who went and looked at it is not a folder anybody can
/// rely on.
function subfolderAsked() {
  if (!subfolderWanted()) return "";
  const chosen = settings.subfolder === null ? autoSubfolder(ready()) : settings.subfolder;
  return filenameSafe(chosen || "");
}

/// The folders this run makes its own folder in.
///
/// The one that was typed where there is one, and otherwise one per folder
/// the recordings came out of: a list gathered from three evenings makes
/// three of these, each beside its own recordings. See `outputBase`.
function outParents() {
  const at = outDir();
  const bare = (p) => p.replace(/[/\\]*$/, "");
  if (at) return [bare(at)];
  const seen = new Set();
  for (const clip of ready()) {
    seen.add(bare(clip.home || dirOf(clip.path)));
  }
  return [...seen];
}

/// The 枝番 the folder has been given, and the half of the question it
/// answers that is cheap to ask again.
///
/// A folder of the name already being there is not something this side can
/// know, so it is asked of the backend -- see `free_folder` -- and the answer
/// is kept, because `subfolderNow` is read once per path on a screen that
/// draws a path per row.
let freeFolder = { dir: null, asked: null, name: "" };

/// The whole question as it was last put, the folders included. Kept apart
/// from the answer because the folders have to be read out of the list, and
/// that is worth doing where the asking happens rather than once per path.
let folderAsked = null;

/// Ask what the folder is really going to be called, where the answer on
/// hand is not about this question. `true` when the name moved, which is the
/// caller's cue to draw again.
///
/// `force` for the run itself: the answer on screen can be minutes old, and
/// a folder can appear in between -- another window, the queue, a hand.
async function askFreeFolder(force = false) {
  if (!invoke || bdavMode()) return false;
  const asked = subfolderAsked();
  if (!asked) return false;
  const dir = outDir();
  const dirs = outParents();
  const key = JSON.stringify([dir, asked, dirs]);
  if (!force && folderAsked === key) return false;
  folderAsked = key;
  let name = asked;
  try {
    name = await invoke("free_folder", { dirs, name: asked });
  } catch {
    // A folder nothing can look at is one this run is about to fail on with
    // a sentence of its own. The plain name, which is what there was before
    // there was a branch.
  }
  const moved = freeFolder.dir !== dir || freeFolder.asked !== asked || freeFolder.name !== name;
  freeFolder = { dir, asked, name };
  return moved;
}

/// The name of the folder this run makes, as it stands right now: what it was
/// asked to be called, with whatever 枝番 that name turned out to need.
///
/// The plain name until an answer is in, which is one redraw and is also the
/// answer in every case where nothing is in the way. A disc is never
/// branched: a second run onto one adds to it, so the folder already being
/// there is the point rather than the problem.
///
/// After a run this is the folder that run wrote, and it stays that until
/// the question is put again -- by the next run, or by a hand on the name or
/// the folder above. The folder the run just made is in the way of the next
/// one, but saying so the instant it is finished would move the path out
/// from under somebody still reading it.
function subfolderNow() {
  if (runFolder !== null) return runFolder;
  const asked = subfolderAsked();
  if (!asked || bdavMode()) return asked;
  return freeFolder.dir === outDir() && freeFolder.asked === asked ? freeFolder.name : asked;
}

/// The folder above, with the one this run makes under it. No trailing
/// separator: this is a folder's path, and the callers add their own.
function beneath(dir) {
  const at = dir.replace(/[/\\]*$/, "");
  const sub = subfolderNow();
  return sub ? `${at}/${sub}` : at;
}

/// Where the disc itself is written: the folder that will hold `BDAV`.
function discDir() {
  return beneath(outDir());
}

/// Where the image will be written, for the panel to show under the disc.
/// Empty when none was asked for, which is most of the time: an image is
/// what you make when the disc is finished and about to be burnt.
function imageLine() {
  if (!settings.image || !outDir()) return "";
  return t(settings.imageOnly ? "outset.imageOnlyLine" : "outset.imageLine", {
    path: `${discDir()}.iso`,
    udf: settings.image,
  });
}

/// The name the engine read out of the programmes, and the list it read it
/// out of: asked once for a list rather than once for every redraw, and the
/// screen and the export then use the one answer.
let discGuess = { of: null, title: "" };

/// The names of the programmes in the list, as one string: what the guess
/// above was made from, and what tells a stale guess from a good one.
function discGuessKey(list) {
  return list.map(programmeOf).join("\n");
}

/// Ask the engine what a disc of these is called. Resolves to a name, and
/// never rejects: a disc wants a title whether or not the reading worked.
///
/// The engine answers with nothing where the recordings do not all name one
/// series, and the disc is then called after the moment it is being made --
/// see `stamp`. That moment is settled here, with the name, and not read off
/// the clock again afterwards: the field on the settings screen shows what
/// will be written, and a title that ticked over while somebody was reading
/// it would be a field that lied about the disc twice a minute.
async function guessDiscTitle(list) {
  // Every recording in the list and not only the one the screen is showing:
  // what the disc is called is a fact about all of them, and a list read half
  // way through looks like a mixture whatever it holds. Once per file -- see
  // `askProgramme` -- and the export would read them anyway.
  for (const clip of list) await askProgramme(clip);
  const key = discGuessKey(list);
  if (discGuess.of === key) return discGuess.title;
  let title = "";
  try {
    title = await invoke("series_title", { names: list.map(programmeOf) });
  } catch {
    title = "";
  }
  discGuess = { of: key, title: title || stamp() };
  return discGuess.title;
}

/// Today, by the clock in the room: `2026-09-11`.
///
/// Put together out of the local parts rather than cut off the front of an
/// `toISOString`, whose date is the one in London. Editing a recording is
/// evening work that runs past midnight, and in Tokyo the two dates differ
/// for the whole of the morning: a folder named for yesterday is one
/// nobody goes looking in.
function today() {
  const now = new Date();
  const two = (n) => String(n).padStart(2, "0");
  return `${now.getFullYear()}-${two(now.getMonth() + 1)}-${two(now.getDate())}`;
}

/// Now, as a disc is willing to be called: `2026-09-11 00:15`.
///
/// Local time, because the moment meant is the one on the clock in the room.
/// To the minute: a disc is not made twice in one, and the seconds would be
/// noise in a recorder's list.
function stamp() {
  const now = new Date();
  const two = (n) => String(n).padStart(2, "0");
  return `${today()} ${two(now.getHours())}:${two(now.getMinutes())}`;
}

/// Now, as ISO 8601 writes a moment that knows where it was:
/// `2026-09-21T08:56:45+09:00`.
///
/// For what goes in a file rather than on the screen. The clock in the
/// room, with the room's distance from UTC beside it, so that the stamp
/// names one moment for anything that reads it and still carries the date
/// somebody would say it was saved on. `toISOString` names the same moment
/// in London: a project saved at one in the morning in Tokyo would be dated
/// the day before, which is the same trick `today` was written to stop.
function stampISO() {
  const now = new Date();
  const two = (n) => String(n).padStart(2, "0");
  // `getTimezoneOffset` counts the other way round -- minutes to add to get
  // to UTC -- and the offset written here is minutes ahead of it.
  const ahead = -now.getTimezoneOffset();
  const mins = Math.abs(ahead);
  const clock = `${two(now.getHours())}:${two(now.getMinutes())}:${two(now.getSeconds())}`;
  const off = `${ahead < 0 ? "-" : "+"}${two(Math.floor(mins / 60))}:${two(mins % 60)}`;
  return `${today()}T${clock}${off}`;
}

/// What to call the disc, when nobody has said.
///
/// The series the recordings are episodes of: six weeks of one programme
/// written onto one disc is the disc most people make, and naming it after
/// the channel they came off -- which is what this did until the engine
/// could take a name apart -- named it after the transponder. A disc of
/// several programmes, or of two seasons of one, has no such name, and is
/// called after the moment it was made.
///
/// The moment is also what stands here until the engine answers, which is a
/// matter of milliseconds. The first programme's name would read better for
/// those milliseconds and worse afterwards -- a disc of a mixture would flash
/// up named after one of them.
function discTitleFor(list) {
  if (settings.discTitle) return settings.discTitle;
  if (!list.length) return "";
  if (discGuess.of === discGuessKey(list) && discGuess.title) return discGuess.title;
  return stamp();
}

wireCrossing();

el("browse-dir").addEventListener("click", async (ev) => {
  ev.preventDefault();
  const picked = await dialog.open({ directory: true, multiple: false });
  if (!picked) return;
  settings.dir = picked;
  settleOutput();
  el("out-dir").value = picked;
  renderOutset();
  renderOutScreen();
  touch();
});

/// Put the screen into the shape the output method asks for.
///
/// A disc names its own files, so a prefix and a container have nothing to
/// choose; its chapter points go into its playlist, so the sidecar has
/// nothing to write; and it has a title and a programme name, which a folder
/// of files has nowhere to put.
function paintMode() {
  const disc = bdavMode();
  for (const b of document.querySelectorAll(".modes .tab")) {
    b.classList.toggle("active", (b.dataset.mode === "bdav") === disc);
  }
  el("row-prefix").hidden = disc;
  el("row-container").hidden = disc;
  el("row-keyframes").hidden = disc;
  // Asked only of a recording that has any: every other one would be
  // answering a question about a kind of subtitle it does not carry. Worded
  // for the disc it came off, because the three answers do different things
  // to a DVD's subtitles than to a Blu-ray's.
  const drawn = drawnSubtitles();
  el("row-subtitles").hidden = !drawn;
  if (drawn) paintSubtitleChoices(drawn);
  // The same rule, for the stream only a receiver reads: asked about where
  // a recording in the list has one and the run is writing the one shape
  // that can hold one.
  // Only where a run would actually use one -- a single file has nothing to
  // be grouped with, and a row offering to make it a folder is a question
  // nobody asked.
  el("row-subfolder").hidden = !subfolderWanted();
  el("row-disc-title").hidden = !disc;
  el("row-disc").hidden = !disc;
  el("row-gauge").hidden = !disc;
  el("row-image").hidden = !disc;
  // The question after it only where there is an image to ask it about.
  el("row-image-access").hidden = !disc || !settings.image;
  el("row-image-only").hidden = !disc || !settings.image;
  el("row-programme").hidden = !disc;
  el("row-channel").hidden = !disc;
  el("row-made").hidden = !disc;
  el("row-about").hidden = !disc;
  el("outset-file-head").textContent = t(disc ? "outset.discHead" : "outset.fileHead");
  el("out-dir-label").textContent = t(disc ? "outset.discFolder" : "outset.outDir");
  el("out-dir").placeholder = t(disc ? "outset.discHere" : "outset.sameAsInput");
}

/// How many digits, only while there is a number to write them in. Greyed
/// rather than hidden: it sits inside the prefix row, and a control coming and
/// going would move the field beside it under the hand.
function paintNumbering() {
  el("out-digits").disabled = !settings.number;
  el("row-digits").classList.toggle("off", !settings.number);
}

// Chosen the way the screens themselves are chosen. The settings on either
// side of the switch are kept, not cleared: coming back to a tab should find
// what was left there.
for (const b of document.querySelectorAll(".modes .tab")) {
  b.addEventListener("click", () => {
    if (settings.mode === b.dataset.mode) return;
    settings.mode = b.dataset.mode;
    settleOutput();
    renderOutset();
    renderOutScreen();
    rememberOutput();
    touch();
  });
}

/// The last name this filled in by itself.
///
/// What tells an untouched field from one somebody typed the same thing
/// into. While the field still holds what was put there, it keeps following
/// what it was made from -- rename the disc and the folder is renamed with
/// it -- and the moment it holds anything else, including nothing, it is the
/// answer and this stops arguing with it.
let filledIn = null;

function settleSubfolder(list) {
  if (subfolderWanted()) {
    const auto = autoSubfolder(list);
    if (auto && (settings.subfolder === null || settings.subfolder === filledIn)) {
      settings.subfolder = auto;
      filledIn = auto;
    }
  }
  // Assigned only when it differs: setting `value` to what it already holds
  // still sends the caret to the end, and this runs on every keystroke.
  const box = el("out-subfolder");
  const want = settings.subfolder ?? "";
  if (box.value !== want) box.value = want;
}

/// Put a value in a field without moving the caret.
///
/// These are redrawn whenever anything about the row changes -- a walk
/// finishing, a lane reporting -- and assigning `value` what it already
/// holds still sends the caret to the end of it. Which, in a box somebody is
/// typing a programme description into, is the screen fighting the hand.
function fill(id, value) {
  const box = el(id);
  if (box.value !== value) box.value = value;
}

/// How much of each text field the disc has room for, said beside it.
///
/// Only the count: the fields are the answer, and this is the margin. Over
/// the limit it turns, because what is over is cut off on the way in and a
/// title that lost its last four characters between the screen and the disc
/// is a title nobody typed.
///
/// The date is here too, which is the same thing said about a different
/// shape: what cannot be read as a moment is written as no moment at all.
function paintIndexFields() {
  const count = (id, room) => {
    const n = aribBytes(el(id).value);
    const box = el(`${id}-bytes`);
    box.textContent = t("outset.bytes", { n, room });
    box.classList.toggle("over", n > room);
    el(id).classList.toggle("over", n > room);
  };
  count("out-programme", ROOM.name);
  count("out-channel", ROOM.channel);
  count("out-about", ROOM.about);
  const made = el("out-made").value.trim();
  const bad = made !== "" && !madeParse(made);
  el("out-made-note").textContent = bad ? t("outset.madeBad") : "";
  el("out-made-note").classList.toggle("over", bad);
  el("out-made").classList.toggle("over", bad);
}

// --- the disc gauge -------------------------------------------------------
//
// What a night's cuts do to a disc. The arithmetic is the engine's -- see
// `smartcut_core::fit` -- and everything it needs was read off each recording
// when it was indexed and travels with the row, so asking costs nothing and
// can be done every time a range moves.

/// The last answer, and what it was an answer about.
///
/// Kept so that the run can use the same share the screen showed. A run works
/// it out again from the list as it stands, because the list can be edited
/// between reading the screen and pressing the button -- this is for drawing.
let room = null;
let roomOf = "";

/// How much of a disc something takes, in gibibytes.
///
/// A disc sold as 25GB holds 25,025,314,816 bytes, and that is the number the
/// engine does its arithmetic against -- but 25.0 is the label on the box
/// rather than a size anything on either platform reports. Counted in 1024s
/// and named GiB, the gauge reads as the same kind of number as the file
/// manager beside it, and the unit says plainly that it is not the 25 on the
/// control above: one names the disc, the other says what is spoken for.
function discGiB(bytes, digits = 1) {
  return `${(bytes / 2 ** 30).toFixed(digits)} GiB`;
}

/// What the list costs the disc, as the engine works it out.
///
/// One entry per ready clip, in list order, so that the bar's segments and
/// the rows line up. A clip that has not been indexed yet has no rates to
/// give and is left out -- it has no cuts either, so there is nothing of it
/// to draw.
function discCosts() {
  return ready()
    .filter((c) => c.info && c.info.video_rate > 0)
    .map((c) => ({
      clip: c,
      seconds: keepsOf(c).reduce((n, k) => n + (k.b - k.a), 0),
      video_rate: c.info.video_rate,
      audio_rate: c.info.audio_rate,
      can_shrink: !!c.info.can_shrink,
    }));
}

/// Ask the engine, unless it has already been asked this exact question.
async function askRoom() {
  const costs = discCosts();
  const capacity = Number(settings.disc) || 25025314816;
  const key = JSON.stringify([capacity, costs.map((c) => [c.seconds, c.video_rate, c.audio_rate, c.can_shrink])]);
  // The stored answer has to be about this list and not the one before it:
  // the key says so, and the count is checked as well because everything
  // drawn from it is drawn per clip.
  if (key === roomOf && room && room.clips.length === costs.length) return { costs, room };
  const answer = await invoke("disc_room", {
    clips: costs.map(({ clip, ...rest }) => rest),
    capacity,
    margin: 0.01,
  });
  roomOf = key;
  room = answer;
  return { costs, room: answer };
}

/// What share the pictures are to be written at for this list to fit, or
/// null where nothing is to be done to them.
///
/// Asked afresh rather than read off the gauge: the list can be edited
/// between looking at the screen and pressing the button.
async function fitShare() {
  if (!bdavMode() || !settings.fit) return null;
  const { room: r } = await askRoom();
  if (!r || r.fits) return null;
  // A list that cannot be reached is still written -- as small as this goes,
  // which is the best answer available to somebody who has pressed the
  // button. The gauge said so before they pressed it, and the output screen
  // says so again when the disc turns out too large for the disc.
  return Math.max(r.share, r.floor);
}

/// Draw it.
async function renderGauge() {
  const box = el("disc-gauge");
  if (!box || el("row-gauge").hidden) return;
  const { costs, room: r } = await askRoom();
  // The screen may have moved on while that was in the air.
  if (el("row-gauge").hidden) return;
  if (!costs.length || !r) {
    box.textContent = t("gauge.empty");
    return;
  }
  // The bar is as long as the larger of the two, so that a list which
  // overflows shows how far past the edge it goes.
  const scale = Math.max(r.bytes, r.capacity, 1);
  const seconds = costs.reduce((n, c) => n + c.seconds, 0);
  const shrinking = settings.fit && !r.fits;

  // A share of the bar, handed to the flex algorithm rather than written as a
  // width. A thousandth of the bar is finer than a pixel at any width this
  // window has.
  const grow = (bytes) => ((Math.max(bytes, 0) / scale) * 1000).toFixed(4);
  const span = (cls, style) => {
    const e = document.createElement("span");
    if (cls) e.className = cls;
    // **Through the DOM, never through the markup.** This window's content
    // policy is `style-src 'self'`, which drops a `style` attribute that
    // arrives as text: the gauge drew as an empty outline with every share
    // sitting correctly in the HTML and every one of them computing to zero.
    // A property set here is not markup and is not dropped.
    if (style) Object.assign(e.style, style);
    return e;
  };

  // One segment per recording, and where a segment falls past the edge of the
  // disc it is drawn as the part that will not fit.
  const bar = (share) => {
    const box = span("gauge-bar");
    let at = 0;
    costs.forEach((c, i) => {
      const clipRoom = r.clips[i] || { bytes: 0, video_bytes: 0, can_shrink: false };
      const bytes = clipRoom.can_shrink
        ? clipRoom.bytes - clipRoom.video_bytes * (1 - share)
        : clipRoom.bytes;
      const over = at + bytes > r.capacity;
      at += bytes;
      const cls = ["gauge-seg", clipRoom.can_shrink ? "" : "fixed", over ? "over" : ""];
      const seg = span(cls.filter(Boolean).join(" "), { flexGrow: grow(bytes) });
      seg.title = `${clipLabel(c.clip)} — ${discGiB(bytes)}`;
      box.appendChild(seg);
    });
    // What is left of the disc, so that the segments keep their share of the
    // whole bar rather than filling it between them.
    box.appendChild(span("gauge-rest", { flexGrow: grow(scale - at) }));
    // The disc's edge, and the margin kept back in front of it, laid over
    // whatever they land on.
    const mark = span("gauge-mark");
    mark.appendChild(span("", { flexGrow: grow(r.usable) }));
    mark.appendChild(span("gauge-edge soft"));
    mark.appendChild(span("", { flexGrow: grow(r.capacity - r.usable) }));
    mark.appendChild(span("gauge-edge"));
    mark.appendChild(span("", { flexGrow: grow(scale - r.capacity) }));
    box.appendChild(mark);
    return box;
  };
  const line = (what, share, bytes) => {
    const row = document.createElement("div");
    row.className = "gauge-line";
    const label = span("gauge-what");
    label.textContent = what;
    const size = span("gauge-size");
    size.textContent = discGiB(bytes);
    row.append(label, bar(share), size);
    return row;
  };

  const after = r.bytes - r.video_bytes * (1 - Math.max(r.share, r.floor));
  // The first bar is labelled as the "before" of a pair only while there is
  // an "after" beside it; on its own it is simply what the list comes to.
  const shown = [line(t(shrinking ? "gauge.asIs" : "gauge.total"), 1, r.bytes)];
  if (shrinking) shown.push(line(t("gauge.fitted"), Math.max(r.share, r.floor), after));

  const words = {
    used: discGiB(r.bytes),
    disc: discGiB(r.capacity),
    pct: ((r.bytes / r.capacity) * 100).toFixed(1),
    n: costs.length,
    dur: coarse(seconds),
    over: discGiB(Math.max(0, r.bytes - r.usable)),
    share: (Math.max(r.share, r.floor) * 100).toFixed(1),
    floor: (r.floor * 100).toFixed(0),
  };
  let note;
  let tone;
  if (r.fits) {
    [note, tone] = [t("gauge.fits", words), "good"];
  } else if (!settings.fit) {
    [note, tone] = [t("gauge.over", words), "over"];
  } else if (r.reachable) {
    [note, tone] = [t("gauge.willFit", words), ""];
  } else {
    [note, tone] = [t("gauge.unreachable", words), "over"];
  }
  // And the one thing the bar cannot say: that some of what is on it will not
  // move whatever is asked of it.
  if (shrinking && costs.some((c) => !c.can_shrink)) note += t("gauge.notMpeg2");
  const say = document.createElement("div");
  say.className = `gauge-note ${tone}`;
  say.textContent = note;
  shown.push(say);
  box.replaceChildren(...shown);
}

/// Whether the list is being written as one file.
///
/// A disc is never that: what a disc holds is recordings, each with its own
/// entry in the index, and a disc of one recording made of twenty is a disc
/// that has lost nineteen names. So the box only means anything in file
/// mode, and this is where the two are asked together.
function joining() {
  return settings.joinAll && !bdavMode();
}

/// The row the joined file takes its shape from.
///
/// The one the setting names, while it is still in the list; the first row
/// otherwise. A list reordered or shortened under a chosen master is the
/// ordinary case -- rows are dragged about -- and a master that has gone is
/// not an error to stop a run over.
function masterClip(list = ready()) {
  return list.find((c) => c.id === settings.master) || list[0] || null;
}

/// What each row of a join has to have done to it to go in beside the master,
/// or null where the run is not a join.
///
/// **The output screen cannot work this out from a plan.** A plan is about
/// the seams a cut leaves -- the partial GOPs at the ends of each kept range
/// -- and it is the same plan whether the clip is written on its own or into
/// another recording's shape. In the second case there is no seam at all:
/// every picture is decoded and made again. So the question goes to the
/// engine, which answers it with the same function the cut itself uses. See
/// `smartcut_core::conform`.
///
/// Held as the promise rather than the answer, so that the half-dozen places
/// that want it while it is still outstanding share the one ask: the settings
/// screen repaints on every keystroke, and the stage asks again for each row
/// the writing head reaches.
let heldFits = null;

function joinFits() {
  const list = ready();
  if (!joining() || list.length < 2) {
    heldFits = null;
    return Promise.resolve(null);
  }
  const paths = list.map((c) => c.path);
  const master = Math.max(0, list.indexOf(masterClip(list)));
  const sig = JSON.stringify([paths, master]);
  if (!heldFits || heldFits.sig !== sig) {
    // A list that cannot be read is not a reason to stop the screen: it goes
    // back to saying what it said before this existed, which promises less
    // but nothing untrue.
    heldFits = { sig, at: invoke("join_fit", { paths, master }).catch(() => null) };
  }
  return heldFits.at;
}

/// The same, for one clip. Matched on the path, because the answer is about a
/// recording: the same file twice in one list is the same answer twice.
async function fitFor(clip) {
  const all = await joinFits();
  if (!all || !clip) return null;
  return all.find((f) => f.path === clip.path) || null;
}

/// Whether a fit is one worth redrawing for, as a string to compare.
///
/// Part of what the stage is showing, beside the clip and its cuts: the same
/// clip with the same cuts says something different once the master under it
/// has changed.
function fitSig(fit) {
  return fit ? JSON.stringify([fit.video, fit.audio, fit.mismatches]) : null;
}

/// Which of a fit's differences are about the pictures.
///
/// Split because the two are said in different places: the pictures on the
/// line under the stage, the sound in the note the output settings put after
/// it. See `audioNote`.
const SOUND_MISMATCH = ["audioCodec", "audioRate", "audioChannels", "audioTracks"];

/// Why a clip does not match, in one phrase.
function whyOf(fit, video = true) {
  if (!fit) return "";
  return fit.mismatches
    .filter((m) => SOUND_MISMATCH.includes(m.what) !== video)
    .map((m) => t("fit.line", { what: t(`fit.${m.what}`), master: m.master, theirs: m.theirs }))
    .join(t("sep"));
}

/// What the crossings come to, beside the button that opens them.
///
/// The button alone would say nothing about what is already set: a list of
/// twelve episodes with a dissolve on every join looks exactly like one with
/// none. So the line says how many joins carry a transition and what that
/// costs the output, which is the one thing about a transition that cannot
/// be seen by looking at it.
function renderCrossing() {
  const row = el("row-cross");
  row.hidden = !joining();
  if (row.hidden) return;
  const list = ready();
  // Every row but the last. A transition belongs to the clip that gives way,
  // and the last one gives way to nothing.
  const joins = list.slice(0, -1);
  el("open-cross").disabled = joins.length === 0;
  if (joins.length === 0) {
    el("cross-note").textContent = t("outset.crossNoJoins");
    return;
  }
  // Either of the two things that happen at a join counts as one being set:
  // a join with no crossing over it and a second of fade under it is a join
  // somebody has settled. See `crossingSet`.
  const set = joins.filter((c) => crossingSet(c.after));
  if (set.length === 0) {
    el("cross-note").textContent = t("outset.crossNoneSet", { of: joins.length });
    return;
  }
  // What the output loses, which only the overlapping kinds take: both clips
  // are on screen at once for those seconds, so the file comes out that much
  // shorter. See `crate::transition`.
  const lost = set
    .filter((c) => OVERLAPPING.includes(c.after.kind))
    .reduce((n, c) => n + Math.min(30, Math.max(0, Number(c.after.seconds) || 0)), 0);
  el("cross-note").textContent =
    lost > 0
      ? t("outset.crossSetShort", { n: set.length, of: joins.length, secs: fmtSecs(lost) })
      : t("outset.crossSet", { n: set.length, of: joins.length });
}

/// The kinds that put both clips on screen at once, which are the ones that
/// take their own seconds off the output. The same split the engine makes;
/// see `Crossing::overlaps`.
const OVERLAPPING = ["dissolve", "wipe-left", "wipe-right", "wipe-top", "wipe-bottom",
                     "slide-left", "slide-right", "slide-top", "slide-bottom"];

const fmtSecs = (n) => (Math.round(Number(n) * 10) / 10).toFixed(1);

// --- 継ぎ目の編集, in its own window --------------------------------------
//
// The same handshake the cut editor has: the window is built in Rust
// (`open_cross`), it says `cross-ready` when its page is up, this window
// answers with `cross-open` naming every join, and OK comes back as
// `cross-done`. キャンセル sends nothing, which is what makes it a cancel --
// the list's own clips are never touched until the answer arrives.

/// Which join the window is to open on. Set by whatever asked for it.
let crossPick = 0;
/// Whether a seam window is being built right now, so that a `cross-closed`
/// landing meanwhile can be told to be about the window before this one.
let crossOpening = false;

/// Every join in the list, in the shape the seam window reads.
///
/// The bounds are what is *kept* at the seam: the last surviving range of
/// one clip and the first of the next. A recording is an hour long and what
/// is being joined may be four minutes of it, so the cuts are what say where
/// the join really falls. See `keepsOf`.
function joinsForWindow() {
  const list = ready();
  const out = [];
  for (let i = 0; i + 1 < list.length; i++) {
    const before = list[i];
    const after = list[i + 1];
    const bk = keepsOf(before);
    const ak = keepsOf(after);
    if (!bk.length || !ak.length) continue;
    const last = bk[bk.length - 1];
    const first = ak[0];
    out.push({
      id: before.id,
      beforePath: before.path,
      afterPath: after.path,
      beforeName: clipLabel(before),
      afterName: clipLabel(after),
      // The picture the row shows, which is the cut-aware one where the
      // pass has produced it and the container's own guess before that.
      beforePic: posterOf(before) || "",
      afterPic: posterOf(after) || "",
      beforeIn: last.a,
      beforeOut: last.b,
      afterIn: first.a,
      afterOut: first.b,
      after: before.after ? { ...before.after } : null,
    });
  }
  return out;
}

function tellCross() {
  if (!emit) return;
  const joins = joinsForWindow();
  if (!joins.length) return;
  emit("cross-open", { joins, pick: Math.min(crossPick, joins.length - 1) });
}

async function openCrossWindow(pick = 0) {
  const joins = joinsForWindow();
  if (!joins.length) {
    note(t("outset.crossNoJoins"));
    return;
  }
  crossPick = Math.min(Math.max(0, pick), joins.length - 1);
  try {
    crossOpening = true;
    await invoke("open_cross", {
      title: t("xw.windowTitle", {
        before: joins[crossPick].beforeName,
        after: joins[crossPick].afterName,
      }),
    });
    // Lost if the window is still starting up, which is what `cross-ready` is
    // for; sent anyway for the case where it is already open and there will
    // be no `cross-ready` at all.
    tellCross();
  } catch (e) {
    note(t("xw.cannotOpen", { e }));
  } finally {
    crossOpening = false;
  }
}

function wireCrossing() {
  el("open-cross").addEventListener("click", () => openCrossWindow(0));
  el("out-master").addEventListener("input", () => {
    const picked = Number(el("out-master").value);
    settings.master = Number.isFinite(picked) ? picked : null;
    settleOutput();
    renderOutset();
    renderOutScreen();
    touch();
  });
}

/// The master picker, and whether the join controls are live at all.
function renderJoin() {
  const list = ready();
  el("row-join").hidden = bdavMode();
  el("out-join").checked = settings.joinAll;
  // Nothing to join with one row, and nothing to be master of. The box is
  // left live all the same -- a list is built up a row at a time, and a box
  // that could only be ticked once the second row was in would be a box
  // nobody found. The picker under it is not: it is a question about a join,
  // and there is no join until the box is ticked and a second row is in.
  const pick = el("out-master");
  const chosen = masterClip(list);
  pick.innerHTML = list
    .map((c, i) => `<option value="${c.id}">${i + 1}: ${esc(clipLabel(c))}</option>`)
    .join("");
  if (chosen) pick.value = String(chosen.id);
  el("row-master").hidden = !joining() || list.length < 2;
  paintMasterNote();
  renderCrossing();
}

/// How much of the list the chosen master costs, beside the picker.
///
/// The picker on its own asks a question nobody can answer: twelve episodes
/// off one recorder are all the same shape and it makes no difference which
/// is picked, while one clip from a phone among them is an hour of encoding
/// that turns on this control. So the line says how many rows do not match,
/// before a run rather than during one.
///
/// Painted from the answer when it arrives. The ask is shared and cached --
/// this runs on every keystroke in the panel -- and a list still being
/// answered for leaves the line as it was rather than blinking through
/// "working it out" a dozen times a second.
let masterNoteToken = 0;
function paintMasterNote() {
  const note = el("master-note");
  if (el("row-master").hidden) {
    note.textContent = "";
    return;
  }
  const token = ++masterNoteToken;
  // Something in the gap where there is nothing yet. Only then: a list that
  // has already been answered for keeps its answer while a repaint goes
  // round, rather than blinking through this on every keystroke.
  if (!note.textContent) note.textContent = t("outset.masterLooking");
  const of = ready().length - 1;
  joinFits().then((fits) => {
    if (token !== masterNoteToken || !fits) return;
    const odd = fits.filter((f) => f.video);
    note.textContent = odd.length
      ? t("outset.masterDiffer", { n: odd.length, of: fits.length })
      : t("outset.masterFits", { n: of });
    paintMasterWhy(odd);
  });
}

/// ...and which rows those are, and what about each of them differs.
///
/// The count on its own is the question rather than the answer. A list of
/// twelve episodes off one recorder with one row that does not match is an
/// hour of encoding, and what to do about it is a different thing in each of
/// the cases it can be: a recording made at another size is one to write out
/// on its own, a recording whose stream states a colour the others leave
/// unstated is one to make the master instead, and a recording with a second
/// sound track is neither -- that difference is in the sound and costs the
/// pictures nothing.
///
/// Said here and not only in the run's own log, which is where it was: the
/// log is read while an hour of encoding is already under way, and this is
/// the screen the hour is agreed to on.
///
/// Every one of them, with no ceiling on the list. A row that does not match
/// is a row somebody is about to spend an hour on; a list where twenty of
/// them differ is a list where the master is the odd one out, and a line
/// saying「ほか 17 件」would hide exactly the case worth seeing.
function paintMasterWhy(odd) {
  const box = el("master-why");
  box.innerHTML = "";
  box.hidden = !odd.length;
  for (const fit of odd) {
    const clip = ready().find((c) => c.path === fit.path);
    const line = document.createElement("div");
    const why = whyOf(fit) || t("fit.unstated");
    line.textContent = t("outset.masterWhy", {
      n: clip ? ready().indexOf(clip) + 1 : "?",
      clip: clip ? clipLabel(clip) : nameOf(fit.path),
      why,
    });
    line.title = line.textContent;
    box.append(line);
  }
}

function renderOutset() {
  lockAudioDetail();
  lockUnwritable();
  lockContainer();
  fillBitrates();
  paintMode();
  paintNumbering();
  renderJoin();
  const list = ready();
  const select = el("outset-clip");
  const was = select.value;
  select.innerHTML = list
    .map((c, i) => `<option value="${c.id}">${i + 1}: ${esc(clipLabel(c))}</option>`)
    .join("");
  if (list.some((c) => String(c.id) === was)) select.value = was;
  const clip = byId(Number(select.value)) || list[0];
  settleSubfolder(list);
  // And what that folder is going to be called once the folders it goes in
  // have been looked at. Asked the way the disc's own name is: not waited
  // for, and it redraws when the answer is in, which the stored answer then
  // stops from asking again.
  askFreeFolder().then((moved) => moved && (renderOutset(), renderOutScreen()));
  // And what it is going to be called, where that is not what the field
  // says. The field keeps the name that was asked for: one that rewrote
  // itself would argue with the hand in it, and a 枝番 put back into the name
  // would be branched again the next time round. So the difference is said
  // beside it instead.
  const asked = subfolderAsked();
  const lands = subfolderNow();
  el("out-subfolder-note").textContent =
    lands && lands !== asked ? t("outset.branched", { name: lands }) : "";
  // Not waited for: it is arithmetic on numbers the window already has, and
  // the rest of the screen has no reason to stand still for it.
  renderGauge();
  const box = el("outset-format");
  if (!clip) {
    box.textContent = t("outset.noReady");
    return;
  }
  if (bdavMode()) {
    // What the recordings say about themselves, asked here rather than when
    // a row was added: it is only this screen that has anything to do with
    // the answer, and a list of forty recordings would otherwise read forty
    // files to draw a list nobody has reached yet. Neither ask is waited
    // for; each redraws the screen when it arrives, and stores its answer
    // before the redraw, which is what stops the redraw asking again.
    //
    // The row on screen first, because that is the half of this screen
    // somebody is looking at. Then the whole list, for the disc's own name,
    // which is worked out from all of them -- the same read per file either
    // way, and whichever gets there first pays for it.
    if (!clip.said) askProgramme(clip).then(() => renderOutset());
    if (discGuess.of !== discGuessKey(list)) guessDiscTitle(list).then(() => renderOutset());
    // Each field shows what will be written, whether that is what somebody
    // typed or what the recording says about itself -- so that reading the
    // screen is reading the disc, and typing is editing rather than
    // guessing. The same reason the disc's own title is filled in below.
    fill("out-programme", programmeOf(clip));
    fill("out-channel", channelOf(clip) ?? "");
    fill("out-channel-number", channelNumberOf(clip) ? String(channelNumberOf(clip)) : "");
    fill("out-made", madeOf(clip) ?? "");
    fill("out-about", descriptionOf(clip) ?? "");
    paintIndexFields();
    if (!settings.discTitle) el("out-disc-title").value = discTitleFor(list);
  }
  const i = clip.info;
  const keeps = keepsOf(clip);
  const kept = keeps.reduce((n, k) => n + (k.b - k.a), 0);
  box.textContent = t(bdavMode() ? "outset.formatBdav" : "outset.format", {
    codec: i.codec,
    w: i.width,
    h: i.height,
    fps: i.fps.toFixed(2),
    scan: t(i.interlaced ? "outset.interlaced" : "media.progressive"),
    audio: audioSummary(clip, containerFor(clip)),
    keeps: keeps.length,
    kept: fmt(kept),
    dur: fmt(i.duration),
    cuts: clip.edit ? clip.edit.cuts.length : 0,
    // What the index will say about the recording is not repeated here: the
    // four fields above are it, and a panel saying the same thing again in
    // grey is a second place to have to keep in agreement with the first.
    marks: chaptersFor(clip).length,
    // Where this recording lands. A joined run has one output for the
    // whole list, so every clip's panel names the same file -- which is the
    // honest answer, and the one somebody would go looking for.
    out: bdavMode()
      ? outDir()
        ? t("outset.discPath", { dir: discDir() }) + imageLine()
        : t("outset.discHere")
      : joining() && ready().length > 1
        ? t("outset.joinedInto", { path: joinedPath(), n: ready().length })
        : outputPath(clip),
    // A joined run writes no sidecar: the marks of twenty recordings on one
    // clock is a list this window has no answer for, and one that named
    // only the first recording's would be worse than none.
    side:
      settings.keyframes
      && !(joining() && ready().length > 1)
      && clip.edit
      && clip.edit.keyframes.length
        ? t("outset.sidecar", {
            path: `${outputPath(clip).replace(/\.[^./\\]*$/, "")}.keyframe`,
          })
        : "",
  });
}
el("outset-clip").addEventListener("change", renderOutset);

// The four things the index says about one recording, which are per clip and
// not per list unlike everything in the panel beside them: what a recording
// is called, which channel it came off, when it went out and what it was
// about are facts about that recording.
//
// The screen is not redrawn as they are typed -- the field is already
// showing what was typed, and redrawing it under the hand is how a caret
// ends up somewhere nobody put it. Only the counts beside them move.
const edits = (id, set) =>
  el(id).addEventListener("input", (ev) => {
    const clip = byId(Number(el("outset-clip").value));
    if (!clip) return;
    set(clip, ev.target.value);
    paintIndexFields();
    touch();
  });

// Emptied, the name goes back to what the recording says about itself rather
// than staying empty -- a nameless row in a recorder's list is the one
// outcome nobody wants. The other three stay empty, because a field a
// recorder leaves blank is a field a disc is allowed to have blank.
edits("out-programme", (clip, v) => (clip.programme = v.trim() ? v : null));
edits("out-channel", (clip, v) => (clip.channel = v));
edits("out-about", (clip, v) => (clip.description = v));
// The three digits and nothing else: 0 -- which is what an empty field
// means -- is the index saying it does not know, which is what a terrestrial
// recording writes there anyway.
edits("out-channel-number", (clip, v) => {
  const n = parseInt(v.replace(/[^0-9]/g, ""), 10);
  clip.channelNumber = Number.isFinite(n) ? Math.min(n, 65535) : 0;
});
edits("out-made", (clip, v) => (clip.made = v.trim()));
// And put back, once the typing has stopped, when what was typed was
// nothing. On the way out of the field rather than on the keystroke that
// emptied it: a name that reappears under a hand still deleting it is a
// field arguing with the person in it.
el("out-programme").addEventListener("change", () => {
  const clip = byId(Number(el("outset-clip").value));
  if (!clip || clip.programme) return;
  fill("out-programme", programmeOf(clip));
  paintIndexFields();
});
// Written back in the one shape the disc uses, once the typing has stopped:
// `2026/8/17 1:00` is a moment a person can type and not one a playlist can
// carry, and turning it into the other on the way past is friendlier than
// refusing it.
el("out-made").addEventListener("change", () => {
  const clip = byId(Number(el("outset-clip").value));
  if (!clip || !clip.made) return;
  const said = madeParse(clip.made);
  if (!said) return;
  clip.made = said;
  fill("out-made", said);
  paintIndexFields();
  touch();
});
// The number is only ever digits, and a field that quietly drops what is
// typed into it is a field that lies. So it is put back as it was kept.
el("out-channel-number").addEventListener("change", () => {
  const clip = byId(Number(el("outset-clip").value));
  if (!clip) return;
  fill("out-channel-number", clip.channelNumber ? String(clip.channelNumber) : "");
});

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

/// The picture that is on the stage at this moment, or null where there is
/// none.
///
/// Kept beside the element rather than read back off it, because the batch
/// tool puts this frame on the row of the queue it belongs to, and that row is
/// drawn from what is known rather than from the screen it is following. See
/// `followJob`.
let stageSrc = null;

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
  box.textContent = onShow.note.text + audioNote(onShow.clip, onShow.fit);
}
/// Keyed on the clip *and its cuts*, so coming back after changing one looks
/// at the new joins rather than the ones that were there before.
let shownReencode = null;
/// And what that showing was told about a disc run's share -- held beside the
/// key rather than in it, because `stillHeld` builds the key from the clip
/// alone and a run's share is not a fact about the clip.
let shownShare = null;
/// The same for what a join has settled about the clip, which changes under
/// it when somebody picks a different master.
let shownFit = null;
let shotsToken = 0;

/// Set when a run ends, to keep the stage where the writing head left it.
///
/// Idle, this screen speaks for the clip about to be written first -- and the
/// moment a run is over that is the top of the list again, so the frame
/// somebody had been watching the encoder make would be swapped, at the very
/// instant it was finished, for one from a clip written minutes ago. The last
/// frame of the run is what the run ended on, and it stays up.
let heldAfterRun = false;

/// Whether that held frame is still about something true. It stops standing
/// for the run the moment the clip leaves the list or its cuts move: the
/// picture was worked out for joins that would no longer be made, and this
/// screen would be showing a plan the program has already dropped.
function stillHeld() {
  if (!heldAfterRun || !onShow) return false;
  const clip = onShow.clip;
  return ready().includes(clip) && shownReencode === JSON.stringify([clip.id, rangesOf(clip)]);
}

/// What share this clip's pictures are actually written at, or null where
/// they are written as they are.
///
/// The run's share is one number for the whole list, and it does not reach
/// every clip of it: the engine can write MPEG-2 back smaller without
/// decoding it and can do that to nothing else, so a recording in anything
/// else is copied at its own size and the disc has to take it. `can_shrink`
/// is the engine's own answer to that question -- see `fit.rs`.
function shrinkShare(clip, share) {
  if (share === null || share === undefined || share >= 1) return null;
  return clip && clip.info && clip.info.can_shrink ? share : null;
}

/// That share as a percentage, the way the gauge and the run's own notes
/// write it.
function sharePct(share) {
  return (share * 100).toFixed(1);
}

async function showReencode(clip, share = null) {
  const token = ++shotsToken;
  if (!clip) {
    onShow = shownReencode = null;
    el("out-shots-note").textContent = "";
    stageShot(null);
    return;
  }
  // The share counts as part of what is on show: the same clip with the same
  // cuts says something different once a run has been told to make the
  // pictures fit a disc, and going by the key alone would leave the line the
  // screen was showing before the button was pressed standing. What a join
  // has settled about the clip counts for the same reason, and counts for
  // more -- it decides whether the plan below describes the file at all --
  // so it is asked first.
  const fit = await fitFor(clip);
  if (token !== shotsToken) return;
  const key = JSON.stringify([clip.id, rangesOf(clip)]);
  const smaller = shrinkShare(clip, share);
  const shape = fitSig(fit);
  if (shownReencode === key && shownShare === smaller && shownFit === shape) return;
  shownReencode = key;
  shownShare = smaller;
  shownFit = shape;
  el("out-shots-note").className = "grow dim";
  el("out-shots-note").textContent = t("out.looking");
  // Nothing to repaint until there is an answer: this runs on into an await,
  // and a `paintShotsNote` in the meantime would put the last clip's line
  // back over "working it out".
  if (onShow) onShow.note = null;
  stageShot(null, t("out.lookingAt", { clip: clipLabel(clip) }));
  // Two answers that do not need a plan, and must not wait for one. A clip
  // with nothing left in it has no segments of any kind; a clip going into
  // another recording's shape has no copied picture for a segment to sit
  // among, so the plan is not about the file being written at all.
  const nothing = { segs: [], shots: [], at: [], out: [], plan: null };
  if (!rangesOf(clip).length) {
    onShow = { clip, r: nothing, fit, at: -1, note: {
      className: "grow dim",
      text: t("out.allCutNote", { clip: clipLabel(clip) }),
    } };
    paintShotsNote();
    stageShot(null, t("out.allCutStage"));
    return;
  }
  if (fit && fit.video) {
    onShow = { clip, r: nothing, fit, at: -1, note: {
      className: "grow conform",
      text: t("out.conformNote", {
        clip: clipLabel(clip),
        master: clipLabel(masterClip()),
        why: whyOf(fit),
      }),
    } };
    paintShotsNote();
    // The clip's own poster, as in the lossless case below: there is no seam
    // to show, and a frame out of the middle of the clip would be standing
    // for a re-encode that covers every other frame just as much.
    stageShot(null, t("out.conformStage"), posterOf(clip));
    return;
  }
  try {
    const r = await reencodeOf(clip);
    if (token !== shotsToken) return;
    onShow = { clip, r, fit, at: -1, note: null };
    const redone = r.segs.reduce((n, g) => n + g.frames, 0);
    if (!r.segs.length) {
      // Cuts that all landed on access points, or no cuts at all. Worth
      // saying rather than leaving it blank: it is the best outcome this
      // program has -- unless the run is also making the pictures smaller to
      // fit a disc, which rewrites every frame of it whatever the cuts did.
      onShow.note = {
        className: smaller === null ? "grow lossless" : "grow dim",
        text: smaller === null
          ? t("out.losslessNote", { clip: clipLabel(clip) })
          : t("out.shrinkNote", { clip: clipLabel(clip), share: sharePct(smaller) }),
      };
      paintShotsNote();
      // The clip's own poster rather than a black rectangle. It does not
      // contradict what this screen is for: the line under it says there is
      // nothing to re-encode, so the picture is standing for the clip about
      // to be written and not for a frame being made again.
      stageShot(
        null,
        smaller === null ? t("out.losslessStage") : t("out.shrinkStage"),
        posterOf(clip)
      );
      return;
    }
    // "everything else is copied byte for byte" is the whole point of the
    // line -- and it is false where the plan came back with no copy in it at
    // all, which is what a range too short to hold an access point comes to.
    const copies = r.plan.segments.some((g) => g.kind === "copy");
    onShow.note = {
      className: "grow dim",
      text: copies
        ? t("out.shots", { clip: clipLabel(clip), n: r.segs.length, frames: redone })
        : t("out.shotsAll", { clip: clipLabel(clip), frames: redone }),
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

/// What the line at the top of the output screen says while a clip is
/// written.
///
/// It used to say the same thing whatever was happening: that the video was
/// being copied losslessly. That is the best case and it is often the true
/// one -- cuts that land on access points cost nothing -- but a cut in the
/// middle of a group of pictures has a stretch either side of it that has to
/// be made again, and a range too short to hold one access point is made
/// again from end to end. Saying "losslessly" over the top of that describes
/// a run nobody is having.
///
/// So it is asked of the plan, which is already in hand by the time a clip
/// starts: the same plan the frames on the stage below came from. A plan that
/// could not be read leaves the plain line standing -- it promises nothing,
/// which is all that can be honestly said there.
/// `fit` is what a join has settled about the clip, and `row` says the line is
/// about one clip of a list being written into one file: it then carries the
/// clip's name, because "copying the video losslessly" over a join of twelve
/// recordings does not say which of the twelve it is true of.
function sayWhatIsWritten(clip, out, share, fit = null, row = false) {
  const name = nameOf(out);
  // Which family of words this line is drawn from. The two say the same
  // things about the same run; the join's carry the clip's name as well.
  const say = (base, said = {}) =>
    t(row ? `out.join${base}` : `out.writing${base}`, {
      name,
      ...(row ? { clip: clipLabel(clip) } : {}),
      ...said,
    });
  // A clip going into another recording's shape is decoded and written afresh
  // from end to end, and the plan below is not about it: it is about seams,
  // and such a clip has none. Asked before everything else for that reason.
  if (fit && fit.video) {
    el("out-state").textContent = say("Conform");
    return;
  }
  // A run that has to fit a disc writes every picture back smaller, and the
  // plan below has nothing to say about that either: the seams are the cheap
  // part of a cut whose whole length is being rewritten. So "losslessly" over
  // the top of a transcode describes a run nobody is having.
  const smaller = shrinkShare(clip, share);
  if (smaller !== null) {
    el("out-state").textContent = say("Shrink", { share: sharePct(smaller) });
    return;
  }
  // The plan this clip is being written to, and not one left over from the
  // ranges it had before somebody moved them.
  const held = clip.reencode;
  const plan =
    held && held.sig === JSON.stringify(rangesOf(clip)) ? held.plan : null;
  const segs = (plan && plan.segments) || [];
  if (!segs.length) return;
  const redone = segs.filter((g) => g.kind !== "copy").length;
  const text = !segs.some((g) => g.kind === "copy")
    ? say("All")
    : redone === 0
      ? say("Copy")
      : say("Most", { n: redone });
  el("out-state").textContent = text;
}

/// The same line, for the row of a join the writing head has just reached.
///
/// Read off what the stage has already worked out for that row rather than
/// asked again: `showReencode` has just settled the plan and the fit, and
/// asking for either of them a second time is a second plan and a second set
/// of decodes for one line of text.
function sayJoinRow(clip) {
  if (!writingJoin || writingTables) return;
  if (!onShow || onShow.clip !== clip) return;
  sayWhatIsWritten(clip, writingJoin.out, writingJoin.share, onShow.fit, true);
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
    stageSrc = poster || null;
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
  stageSrc = shot ? shot.url : null;
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

/// Where each row of a join falls in the one file, as a pair of fractions.
///
/// By how long each row's kept ranges are, which is near enough what the
/// engine counts its pictures over. Near enough rather than exact: a crossing
/// is written out of the two rows it joins and belongs to neither of them, so
/// a list with dissolves in it moves a fraction of a second under the head.
/// Nothing that can be seen on a bar.
function joinParts(list) {
  const lens = list.map((c) => keepsOf(c).reduce((n, k) => n + (k.b - k.a), 0));
  const whole = lens.reduce((n, l) => n + l, 0) || 1;
  let at = 0;
  return list.map((clip, i) => {
    const start = at;
    at += lens[i] / whole;
    // The last row ends at the end of the file whatever the arithmetic came
    // to, so that a finished run leaves no row a hundredth short.
    return { clip, start, end: i === list.length - 1 ? 1 : at };
  });
}

/// Follow the head through a join: fill the rows it has passed, fill the one
/// it is in as far as it has got, and put that row on the stage.
///
/// `head` is how far through the writing the engine is, over the whole list.
/// Returns whether anything a row shows has changed, which is what decides
/// whether the screen is drawn again: the job's own percentage is too coarse
/// to decide it for a join, because a row is a twelfth of the job and moves
/// twelve times as fast as it does.
function followJoin(head) {
  let moved = false;
  for (const part of writingJoin.parts) {
    const span = Math.max(part.end - part.start, 1e-9);
    const at = clamp((head - part.start) / span, 0, 1);
    const was = part.clip.out;
    // The same three states a row goes through in any other run, so the list
    // reads the way it does when each row is a file of its own.
    const state = at >= 1 ? "done" : at > 0 ? "running" : "waiting";
    if (state !== was.state || Math.round(at * 100) !== Math.round(was.progress * 100)) {
      moved = true;
    }
    part.clip.out = {
      state,
      progress: at,
      note: at > 0 ? `${Math.round(at * 100)}%` : t("out.waiting"),
    };
  }
  // The row the head is in, which is the first one it has not finished.
  const parts = writingJoin.parts;
  const part = parts.find((p) => head < p.end) || parts[parts.length - 1];
  writingJoin.at = part;
  // The stage holds one recording's frames, and a join walks through every
  // recording in the list: a stage left on the first of them would be showing
  // a frame that was written minutes ago. Asked for once per row and not
  // awaited -- it is a plan and some decodes, and the reports keep coming
  // while they are made. Once per row rather than on every report even while
  // it is outstanding: a row the frames cannot be got out of would otherwise
  // be asked for twice a second for as long as it took to write.
  if (writingJoin.asked !== part.clip) {
    writingJoin.asked = part.clip;
    const row = part.clip;
    // And the line at the top with it, once the stage has an answer: what is
    // being done to the pictures is a different answer for every row of a
    // join -- one copied, the next written afresh to fit the master's shape
    // -- and the line used to be settled once, from the first row, and stand
    // through all of them.
    showReencode(row, writingJoin.share).then(() => {
      if (writingJoin && writingJoin.asked === row) sayJoinRow(row);
    });
  }
  if (!onShow || onShow.clip !== part.clip) return moved;
  const span = Math.max(part.end - part.start, 1e-9);
  followWrite(clamp((head - part.start) / span, 0, 1));
  return moved;
}

// --- output -------------------------------------------------------------

let exporting = false;
let abort = false;
/// The row being written, so a progress event can be told apart from a stale
/// one belonging to the row before it.
let writing = null;
/// The rows of a join, with the stretch of the one file each of them holds.
/// Null for every other run.
///
/// A list written a file apiece has one row under the head at a time, and the
/// engine's report is about that row: the report *is* the row's progress. A
/// join is one report over the whole list, because it is one file. Put onto
/// the first row alone it left the other eleven sitting at nought until the
/// run ended, and the run's own bar -- worked out as one row of twelve --
/// stopped at a twelfth and then jumped to full. So a join's report is read
/// as a position in the list here, and the rows are filled from where it
/// falls. See `joinParts`.
let writingJoin = null;
/// Whether the row being written is in its second pass -- the one that puts
/// the broadcast's own tables back. Held so the sentence is written once, on
/// the report that crosses over, rather than on every one after it.
let writingTables = false;
/// What that row's output file is called, for the sentence the second pass
/// writes. Held because the report carries the recording's path and the
/// sentence is about the file being written.
let writingName = "";
let began = 0;
/// What a disc run does after the last cut: the index the recordings are
/// wrapped in, and the image the folder is wrapped in. Both are minutes of
/// work on a disc's worth of material, so they are rows in the list and turns
/// on the bar rather than a percentage in the status line -- a bar that sat
/// full while the disc was still being written was saying the run was over.
let discSteps = [];
/// When the stretch the bar is currently about began, which is the run for
/// the cuts and the step itself for each of those two. What is left is worked
/// out from it; what has gone is always the whole run.
let phaseBegan = 0;

function discStep(key) {
  return discSteps.find((s) => s.key === key);
}

/// A step is over. Done fills its bar, because a pass that reports in
/// thousandths stops somewhere short of the end; failed leaves the bar where
/// it stopped, which is where it went wrong.
function finishStep(key, state) {
  const step = discStep(key);
  if (!step) return;
  step.state = state;
  if (state === "done") {
    step.progress = 1;
    step.note = "100%";
    paintOutProgress(1, phaseBegan);
  } else {
    step.note = t("out.stepFailed");
    // Whatever was to come after it is not coming.
    for (const s of discSteps) {
      if (s.state === "waiting") {
        s.state = "skipped";
        s.note = t("out.skipped");
      }
    }
  }
  renderOutScreen();
}

function renderOutScreen() {
  // The same ask the settings screen makes, because this screen shows the
  // path too and a run can be started from here without that one ever having
  // been drawn. See `askFreeFolder`.
  askFreeFolder().then((moved) => moved && renderOutScreen());
  // Where the files actually land, folder of their own included: this line
  // is read while the run is watched, and a path that is one level off the
  // one being written to is worse than no line at all. With no folder
  // chosen there is no one path to write -- the cuts go beside the
  // recordings they were made from, which can be three folders -- so the
  // folder of their own is named beside the phrase rather than left out of
  // it, which was this line saying the cuts land somewhere they do not.
  const sub = subfolderNow();
  el("out-dir-shown").value = bdavMode()
    ? outDir()
      ? t("outset.discPath", { dir: discDir() })
      : t("outset.discHere")
    : outDir()
      ? beneath(outDir())
      : sub
        ? t("outset.sameAsInputSub", { name: sub })
        : t("outset.sameAsInput");
  const list = ready();
  el("out-idle").hidden = list.length > 0;
  // Idle, the screen speaks for whichever clip is about to be written first;
  // running, `runExport` points it at the one under the head. Just finished,
  // it stays on the frame the head stopped at.
  if (!exporting && !stillHeld()) {
    const first = list[0] || null;
    // With the share the run would be given, where there is one to have: a
    // list that has to be made smaller to fit its disc is not going to be
    // copied losslessly, and saying so the moment before the button is
    // pressed is the same untruth as saying it while the run goes. The ask
    // is the gauge's own and its answer is cached, so it is put only when
    // the box that makes it matter is ticked.
    if (bdavMode() && settings.fit) {
      fitShare().then((share) => !exporting && showReencode(first, share));
    } else {
      showReencode(first);
    }
  }
  // The picture half of the note is cached against the clip and its cuts;
  // this puts the audio half back on it, which the settings can have changed
  // since.
  paintShotsNote();
  const rows = el("out-list");
  // The cuts, and then what the disc is wrapped in. The two come out of the
  // same shape as a cut because they are the same thing to whoever is
  // watching: a named piece of the run, with how far through it is.
  const shown = [
    ...list.map((c, i) => ({
      n: String(i + 1),
      name: clipLabel(c),
      len: fmt(keepsOf(c).reduce((n, k) => n + (k.b - k.a), 0)),
      out: c.out,
    })),
    ...discSteps.map((s) => ({ n: "", name: s.label, len: "", out: s })),
  ];
  rows.innerHTML = shown
    .map(
      (s) => `<li class="${s.out.state}">
        <span class="n">${s.n}</span>
        <span class="nm">${esc(s.name)}</span>
        <span class="len dim">${s.len}</span>
        <span class="pbar"><span></span></span>
        <span class="note dim">${esc(s.out.note || "")}</span>
      </li>`
    )
    .join("");
  // The bar is filled from here rather than written into the markup above: a
  // `style` attribute in markup is the one thing the window's content policy
  // turns off, and the property is not.
  shown.forEach((s, i) => {
    const bar = rows.children[i]?.querySelector(".pbar span");
    if (bar) bar.style.width = `${Math.round(s.out.progress * 100)}%`;
  });
  followRow(rows, shown.findIndex((s) => s.out.state === "running"));
  paintButtons();
}

/// Keep the row under the writing head where it can be seen.
///
/// The list shows three rows at a time and a disc's run is a dozen or twenty
/// long, so the row that is actually moving spends most of a run below the
/// fold -- and the markup above is rebuilt on every progress report, which
/// puts a scroll back at the top as fast as anybody could set it.
///
/// Middled rather than merely brought into view, so that what is around it
/// is readable too: the row above is what was just written and the row below
/// is what comes next, and those are the two things somebody looking at a
/// run wants beside the one in hand. `offsetTop` is the row's place in the
/// list because the list is positioned; see `#out-list` in the stylesheet.
function followRow(rows, at) {
  const row = at >= 0 ? rows.children[at] : null;
  if (!row) return;
  const middle = row.offsetTop - (rows.clientHeight - row.offsetHeight) / 2;
  rows.scrollTop = Math.max(0, Math.min(middle, rows.scrollHeight - rows.clientHeight));
}

/// Paint the bar for the stretch of the run it is currently about.
///
/// `since` is when that stretch began: the run itself while the cuts are
/// being written, and the step itself for each of the two a disc is finished
/// with. Time gone is always the whole run -- somebody watching wants to know
/// how long they have been waiting, not how long this pass has -- and time
/// left is worked out inside the stretch, because the rate of one says
/// nothing about the rate of the next.
function paintOutProgress(overall, since = began) {
  const pct = Math.round(overall * 100);
  el("progress-bar").style.width = `${pct}%`;
  el("out-pct").textContent = `${pct}%`;
  el("out-elapsed").textContent = t("out.elapsed", {
    t: clock((Date.now() - began) / 1000),
  });
  const spent = (Date.now() - since) / 1000;
  el("out-left").textContent =
    overall > 0.01
      ? t("out.left", { t: clock((spent / overall) * (1 - overall)) })
      : t("out.leftUnknown");
  // Every pass of a run reports through here -- the cuts, the index, the
  // image -- so this is where the row of the queue the run belongs to hears
  // about all three. See `paintJobProgress`.
  paintJobProgress(overall, since);
}

if (listen) {
  listen("export-progress", (ev) => {
    const [path, tables, done, within] = ev.payload;
    if (!writing || writing.path !== path) return;
    const clip = writing;
    // Whether anything anybody can see has moved since the last report. What
    // it saves is the whole row list rebuilt from markup plus a layout read
    // to keep the moving row in view; the reports come in twice a second
    // either way.
    let moved;
    // Outside the guard below, both of them: this is what moves the stage on
    // to the next stretch of the cut, and a stretch can begin between two
    // whole percent.
    //
    // `within` rather than `done`, and only while the pictures are being
    // written. The two are the same number only for a cut with no second
    // pass: `done` is the whole job, so on a `.ts` the stage reached seven
    // tenths of the way through the cut while it was being written and then
    // walked the rest of it during the pass that puts the tables and the
    // data broadcast in, where there is nothing left to encode.
    if (writingJoin) {
      const was = Math.round(writingJoin.done * 100);
      writingJoin.done = done;
      // The head's place in the list. The second pass is over the finished
      // file rather than over any one row, so the rows stand full through it
      // -- which leaves the job's own figure as the only thing still moving,
      // so it is asked as well as the rows.
      moved = followJoin(tables ? 1 : within);
      if (Math.round(done * 100) !== was) moved = true;
    } else {
      const was = Math.round(clip.out.progress * 100);
      clip.out.progress = done;
      clip.out.note = `${Math.round(done * 100)}%`;
      if (!tables) followWrite(within);
      moved = Math.round(done * 100) !== was;
    }
    // The second pass over the file, which a `.ts` always has: the tables the
    // muxer cannot write, put back over the ones it did. It is a read and a
    // write of the whole finished file, so a window still saying 出力中
    // through it is saying the wrong thing for a third of the run.
    if (tables !== writingTables) {
      writingTables = tables;
      if (tables) {
        el("out-state").textContent = t("out.writingTables", { name: writingName });
      }
    } else if (!moved) {
      return;
    }
    renderOutScreen();
    const all = ready();
    const finished = all.filter((c) => c.out.state === "done").length;
    // A join's report already covers the whole list, because the list is one
    // file; every other run reports a row at a time, so the rows already
    // written are counted in beside it.
    const overall = writingJoin ? done : all.length ? (finished + done) / all.length : 0;
    paintOutProgress(overall);
  });
}

if (listen) {
  // The image says how far through it is once for every megabyte it copies,
  // which on a disc is twenty thousand times. Painting a whole list for each
  // of those is work nobody can see: the screen only changes when the
  // rounded percentage does.
  //
  // `said` is the line the status is to be left on, and it goes up before the
  // bars are painted rather than after: the batch tool's card reads that line
  // as the run moves -- see `followJob` -- and a card painted first would be a
  // card a step behind the step it is about.
  const stepped = (step, done, said) => {
    const was = Math.round(step.progress * 100);
    step.progress = done;
    step.note = `${Math.round(done * 100)}%`;
    if (Math.round(done * 100) === was) return;
    el("out-state").textContent = said;
    paintOutProgress(done, phaseBegan);
    renderOutScreen();
  };

  listen("image-progress", (ev) => {
    if (!exporting) return;
    const step = discStep("image");
    if (!step) return;
    stepped(
      step,
      ev.payload,
      t("out.imaging", { udf: settings.image, pct: Math.round(ev.payload * 100) })
    );
  });

  // The pass that writes the disc's index reads every stream back, which on
  // a disc's worth of recordings is minutes. It reports one recording at a
  // time; the bar is about the whole step, so the recordings before this one
  // are counted into it.
  listen("bdav-progress", (ev) => {
    const [clip, done] = ev.payload;
    if (!exporting) return;
    const step = discStep("index");
    if (!step) return;
    const k = step.clips.indexOf(clip);
    const overall = k < 0 ? done : (k + done) / step.clips.length;
    stepped(step, overall, `${t("out.bdavIndexing", { clip })} ${Math.round(done * 100)}%`);
  });
}

/// The one button on the 出力 screen, which says what pressing it now does.
///
/// 出力中止 while a run is on and 出力開始 the rest of the time. Greyed out
/// with nothing to write, while the queue is being written by the tool -- and
/// while a stop is already on its way, because 中止 twice is not twice as
/// stopped and the state line has just said so.
///
/// Hidden altogether in the batch tool, which has its own control on the bar
/// over a queue rather than over one list; see the startup code.
function paintExportButton() {
  const button = el("run-export");
  if (!button || button.hidden) return;
  button.textContent = t(exporting ? "out.abort" : "out.run");
  button.disabled = exporting ? abort : ready().length === 0 || batchRunning;
}

/// The button beside it, which is about the queue rather than about now.
///
/// バッチに登録 in a window with a list of its own, バッチを上書き in one that
/// was opened on a job out of the queue -- the same act, said about the file
/// that window is holding. Written here rather than marked up for the reason
/// the button above it is: the label is what pressing it does, not what the
/// button is called.
function paintEnlistButton() {
  const button = el("enlist-export");
  if (!button || button.hidden) return;
  button.textContent = t(queuedJob ? "out.overwrite" : "out.enlist");
  button.disabled = clips.length === 0 || exporting || batchRunning;
}

el("run-export").addEventListener("click", () => {
  if (!exporting) {
    runExport();
    return;
  }
  abort = true;
  el("out-state").textContent = t("out.aborting");
  paintExportButton();
});

/// Write the whole list as one file.
///
/// Returns whether it landed. Every row carries the run's state, because
/// every row is in the file: a list of twelve that failed is twelve rows
/// that were not written, and one of them showing an error while the other
/// eleven said nothing would be eleven rows lying about what happened.
async function writeJoined(list) {
  const out = joinedPath();
  const clash = list.find((c) => c.path === out);
  if (clash) {
    for (const c of list) c.out = { state: "error", progress: 0, note: t("out.sameName") };
    renderOutScreen();
    return false;
  }
  const empty = list.find((c) => !rangesOf(c).length);
  if (empty) {
    for (const c of list) {
      c.out = c === empty
        ? { state: "error", progress: 0, note: t("out.allCut") }
        : { state: "skipped", progress: 0, note: t("out.skipped") };
    }
    renderOutScreen();
    return false;
  }
  const share = await fitShare();
  // The run is drawn on the first row, because the bar the engine reports
  // against is tagged with the first recording -- see `export_joined`.
  writing = list[0];
  // And every row of it is in the file the reports are about, at a known
  // place: see `writingJoin`. The share goes in beside them because the stage
  // is moved from row to row as the head passes, and what a recording's
  // pictures are written at is part of what the stage says about it.
  // `asked` starts at nothing rather than at the first row: the first report
  // is what puts that row's line up, and a row this already claimed to have
  // asked for would never get one. The ask itself costs nothing -- the stage
  // is showing that row already and `showReencode` sees as much.
  writingJoin = { parts: joinParts(list), at: null, asked: null, done: 0, share, out };
  writingTables = false;
  writingName = nameOf(out);
  for (const c of list) c.out = { state: "waiting", progress: 0, note: t("out.waiting") };
  el("out-state").textContent = t("out.writing", { name: writingName });
  renderOutScreen();
  await showReencode(list[0], share);
  sayWhatIsWritten(list[0], out, share, onShow && onShow.fit, true);
  // And over the top of it, where there are crossings: a transition is
  // seconds of encoding that belong to no clip's ranges at all, so no row's
  // line will ever mention it. This is the run's own sentence and it stands
  // until the first report arrives -- which is not the instant it goes up:
  // every recording of the list is opened and planned before a byte is
  // written, and on a dozen of them that is the several seconds this is on
  // screen for. After that the line belongs to the row under the head, which
  // is the thing that is actually happening.
  const crossings = list.slice(0, -1).filter((c) => c.after && c.after.kind !== "none");
  if (crossings.length) {
    const secs = crossings.reduce((n, c) => n + Number(c.after.seconds || 0), 0);
    el("out-state").textContent = t("out.writingCrossings", {
      name: writingName,
      n: crossings.length,
      secs: fmtSecs(secs),
    });
  }
  if (onShow && onShow.r.segs.length) stageShot(0);
  const master = masterClip(list);
  try {
    await invoke("export_joined", {
      clips: list.map((c) => ({
        path: c.path,
        ranges: rangesOf(c),
        dropStreams: c.edit ? c.edit.dropStreams || [] : [],
        dropPids: c.edit ? [] : c.dropPids,
        // The last row has nothing to give way to, so whatever it carries is
        // not sent: the engine would read it as a fade to black at the end
        // of the file, which is a thing to ask for rather than to inherit
        // from 一括適用.
        after: c === list[list.length - 1] ? null : c.after,
      })),
      master: Math.max(0, list.indexOf(master)),
      output: out,
      audioCopy: settings.audio === "copy",
      audioReencode: settings.audio === "reencode",
      audioCodec: audioCodecOut(),
      audioChannels: audioChannelsOut(),
      audioBitrate: audioBitrateOut(),
      audioSampleRate: audioRateOut(),
      audioBits: audioBitsOut(),
      subtitles: settings.subtitles,
      dataBroadcast: prefs.get("dataBroadcast") !== false,
      videoShare: share,
    });
    followWrite(1);
    for (const c of list) c.out = { state: "done", progress: 1, note: t("out.done", { extra: "" }) };
    note(t("out.joined", { n: list.length, name: nameOf(out) }));
    return true;
  } catch (e) {
    for (const c of list) c.out = { state: "error", progress: 0, note: String(e) };
    return false;
  } finally {
    writing = null;
    writingJoin = null;
    renderOutScreen();
    paintOutProgress(1);
  }
}

async function runExport() {
  if (exporting) return;
  // Nothing to collect from the editor first: it reports every change as it
  // makes it, so what the list holds is already what is on screen in there.
  const list = ready();
  if (!list.length) return;
  // The last run's sentence goes down before this one has anything to say.
  // It stands after a run ends -- which is what makes it readable at all --
  // and a folder the *previous* run was given a branch number for, left on
  // screen while this one writes, is a sentence about the wrong run.
  note("");
  // A disc is built somewhere. There is no "beside the input" for one: the
  // recordings in a list can come from four folders and a disc is one place.
  const disc = bdavMode();
  if (disc && !settings.dir) {
    note(t("out.needDiscFolder"));
    show("outset");
    el("out-dir").focus();
    return;
  }
  // A moment the playlist cannot carry is written as no moment at all, and a
  // recording that lost the night it went out somewhere between the field and
  // the disc is worth stopping for -- it is one keystroke to fix and an hour
  // of writing to find out about afterwards.
  const unreadable = disc && list.find((c) => madeOf(c) && !madeParse(madeOf(c)));
  if (unreadable) {
    note(t("out.madeUnreadable", { name: programmeOf(unreadable) }));
    show("outset");
    el("outset-clip").value = String(unreadable.id);
    renderOutset();
    el("out-made").focus();
    return;
  }
  // A pass over another recording would be competing for the same disc, and
  // unlike the editor this is work with an end in sight that somebody is
  // watching. Both lanes stand aside until the list is written out.
  paused = true;
  await invoke("stop_batch", { lane: null });
  // What each recording will be called on the disc, and what the disc is
  // called. Asked before the folder below is settled, not after: the folder
  // a disc is written into is named after the disc, and a name worked out a
  // moment too late would leave the folder called something else.
  if (disc) {
    for (const clip of list) await askProgramme(clip);
    await guessDiscTitle(list);
  }
  // Settled before the first byte and held until the last: see `runDir`.
  runDir = settings.dir;
  // Asked again here rather than taken off the screen: what is on screen can
  // be minutes old, and a folder of that name can have appeared in between
  // -- another window, the queue, a hand. A disc is not asked at all; a
  // second run onto one adds to it.
  const asked = subfolderAsked();
  if (!disc) await askFreeFolder(true);
  runFolder = subfolderNow();
  // Said rather than done quietly. A run that wrote somewhere other than
  // where the screen had been saying is a run somebody would go looking for
  // in the wrong folder.
  if (runFolder !== asked) note(t("out.branched", { asked, name: runFolder }));

  // And under which number. Settled before anything is written: the
  // numbering depends on what the disc already holds, and asking a recording
  // what programme it holds is a read of it that should not happen between
  // two cuts.
  let slots = null;
  if (disc) {
    try {
      slots = await invoke("bdav_prepare", { dir: discDir(), n: list.length });
    } catch (e) {
      // Nothing has been written yet, so this is a run that did not start
      // rather than one that failed part way: the lanes get their turn back
      // and the list is as it was.
      note(t("out.bdavFailed", { e: String(e) }));
      runDir = null;
      runFolder = null;
      paused = false;
      pump();
      return;
    }
  }

  // What the pictures have to come to for this list to fit the disc, worked
  // out once for the whole run: the recordings share a disc, so they share
  // the answer. A run that asked per clip would give the long one a harder
  // time than the short one for no reason but its length.
  const share = await fitShare();
  if (share !== null) note(t("out.shrinking", { share: sharePct(share) }));

  exporting = true;
  abort = false;
  began = Date.now();
  phaseBegan = began;
  discSteps = [];
  heldAfterRun = false;
  list.forEach((c) => (c.out = { state: "waiting", progress: 0, note: t("out.waiting") }));
  paintButtons();
  renderOutScreen();

  let done = 0;
  // **One file, written once.** Everything below this is the same run seen
  // from the other side: the disc pass is skipped -- a disc holds
  // recordings, and joining is the one thing it cannot do -- and the
  // summary, the bar and the folder are the run's either way.
  if (joining() && list.length > 1) {
    done = (await writeJoined(list)) ? list.length : 0;
  } else
  for (const [i, clip] of list.entries()) {
    if (abort) {
      clip.out = { state: "skipped", progress: 0, note: t("out.skipped") };
      continue;
    }
    // A recording on a disc is `BDAV/STREAM/00001.m2ts`; which number it is
    // was settled above. What it is *called* goes in the index, not here.
    const out = disc ? slots[i].path : outputPath(clip);
    if (out === clip.path) {
      clip.out = { state: "error", progress: 0, note: t("out.sameName") };
      renderOutScreen();
      continue;
    }
    // A clip whose cuts cover the whole recording. The engine refuses it too
    // -- nothing is kept, so there is no file to write -- but it is said
    // here, in the words the editor used when the last cut closed the last
    // range, rather than handed back as an engine's sentence at the end of a
    // run that looked like it was going to write something.
    if (!rangesOf(clip).length) {
      clip.out = { state: "error", progress: 0, note: t("out.allCut") };
      renderOutScreen();
      continue;
    }
    writing = clip;
    // A fresh row is in its first pass, whatever the row before it ended in.
    writingTables = false;
    writingName = nameOf(out);
    clip.out = { state: "running", progress: 0, note: "0%" };
    el("out-state").textContent = t("out.writing", { name: writingName });
    renderOutScreen();
    // Before the cut starts, not during: the plan and the frames are reads of
    // the same recording the cut is about to stream off the disc.
    await showReencode(clip, share);
    // And now that the plan is in, the line above can say what is actually
    // being written rather than the best case.
    sayWhatIsWritten(clip, out, share);
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
        audioCodec: audioCodecOut(),
        audioChannels: audioChannelsOut(),
        audioBitrate: audioBitrateOut(),
        audioSampleRate: audioRateOut(),
        audioBits: audioBitsOut(),
        // What the editor's track menu switched off for this clip. Per clip
        // and not per list: the audio settings above are one answer for the
        // whole run, but which of a recording's own streams are wanted is a
        // fact about that recording.
        dropStreams: clip.edit ? clip.edit.dropStreams || [] : [],
        // And what the chooser switched off when the disc was read, for a row
        // nobody has opened the editor on. Only then: once there is an edit,
        // the track menu's answer is the answer, and sending both would let a
        // track switched back on in the editor be switched off again here.
        dropPids: clip.edit ? [] : clip.dropPids,
        subtitles: settings.subtitles,
        // A standing answer rather than one of this project's: the box is
        // in 環境設定. See `prefs.dataBroadcast`.
        dataBroadcast: prefs.get("dataBroadcast") !== false,
        // And what the disc has room for. The same number for every clip of
        // the run; null where the list fits as it is, which is every run
        // that is not going onto a disc.
        videoShare: share,
      });
      // The head is past everything now, so the stage catches up with it: the
      // frame left standing is the last one the encoder made, rather than
      // whichever one the last progress report happened to fall short of.
      followWrite(1);
      let extra = "";
      // A disc carries its chapter points in its own playlist, which is
      // where a player looks for them; a file beside the stream would be the
      // same list written twice, in the one place nothing reads.
      if (settings.keyframes && !disc) {
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

  // The streams are written; a disc is what they are written *into*, and
  // that is this pass. Only the recordings that actually landed: a slot
  // whose cut failed has no stream to be a playlist about. And a run that
  // was stopped gets no index at all -- the pass reads every stream back and
  // is minutes of work nobody asked for once they have said stop.
  if (disc) {
    const pairs = list.map((clip, i) => ({ clip, slot: slots[i] }));
    const wrote = abort ? [] : pairs.filter(({ clip }) => clip.out.state === "done");
    // Which leaves streams no playlist will ever name: gigabytes of a
    // recording the disc does not know it has, skipped by the next run's
    // numbering and carried into any image made of the disc afterwards.
    // They are taken back, so a disc holds what it says it holds.
    const dropped = pairs.filter((p) => !wrote.includes(p)).map(({ slot }) => slot.clip);
    if (dropped.length) {
      try {
        const gone = await invoke("bdav_discard", { dir: discDir(), clips: dropped });
        // And the rows are told. A recording whose stream has been taken
        // back is not one this run wrote, whatever it said a moment ago.
        for (const p of pairs) {
          if (!wrote.includes(p) && p.clip.out.state === "done") {
            p.clip.out = { state: "skipped", progress: 0, note: t("out.discarded") };
            done--;
          }
        }
        renderOutScreen();
        if (gone) note(t("out.bdavDiscarded", { n: gone }));
      } catch (e) {
        // Worth saying and not worth stopping for: the disc is written
        // either way, and what is left behind is a file somebody can delete.
        note(t("out.bdavLeftover", { e: String(e) }));
      }
    }
    if (wrote.length) {
      // The rest of the run, as rows, before the first of it is asked for:
      // what is still to come is part of what somebody is watching, and a
      // list that grew a row at a time would keep moving under them.
      discSteps = [
        {
          key: "index",
          label: t("out.stepIndex"),
          clips: wrote.map(({ slot }) => slot.clip),
          state: "running",
          progress: 0,
          note: "0%",
        },
      ];
      if (settings.image) {
        discSteps.push({
          key: "image",
          label: t("out.stepImage", { udf: settings.image }),
          state: "waiting",
          progress: 0,
          note: t("out.waiting"),
        });
      }
      phaseBegan = Date.now();
      paintOutProgress(0, phaseBegan);
      el("out-state").textContent = t("out.bdavIndexing", { clip: wrote[0].slot.clip });
      renderOutScreen();
      try {
        const wroteBytes = await invoke("bdav_finish", {
          dir: discDir(),
          title: discTitleFor(list),
          entries: wrote.map(({ clip, slot }) => ({
            clip: slot.clip,
            name: programmeOf(clip),
            // Each of the three in the one shape the playlist has for it, and
            // nothing where the field was left empty: a moment as the disc
            // spells it, whatever the field was typed in, and a text or no
            // text rather than a text of no length.
            made: madeParse(madeOf(clip) || ""),
            // Not one of the four anybody types: the length the listing gave
            // the programme, carried from wherever the recording knew it.
            ran: clip.ran ?? (clip.said || {}).ran ?? null,
            description: descriptionOf(clip) || null,
            channel: channelOf(clip) || null,
            // Named the way the engine names it: the fields of a payload go
            // across as they are written, unlike a command's own arguments.
            channel_number: channelNumberOf(clip),
            marks: chaptersFor(clip),
          })),
        });
        finishStep("index", "done");
        note(
          t("out.bdavDone", {
            path: `${discDir()}/BDAV`,
            n: wrote.length,
          })
        );
        // And what it came to, against the disc it was meant for. The gauge
        // said what was expected before the run started; this is what
        // happened. They part company where the pictures would not shrink as
        // far as they were asked to -- which is a thing this program can only
        // find out by trying, and which is worth knowing before a disc is
        // burned rather than after.
        const capacity = Number(settings.disc) || 0;
        if (wroteBytes > 0 && capacity > 0) {
          // A disc that came out too large says so either way, but only a
          // run that was asked to make the pictures smaller can say they
          // would go no smaller. Where nothing was asked of them, what is
          // worth saying is that asking is there to be done.
          let said = "out.discSize";
          if (wroteBytes > capacity) {
            said = share !== null ? "out.discTooBig" : "out.discOver";
          }
          note(
            t(said, {
              used: discGiB(wroteBytes, 2),
              disc: discGiB(capacity),
              over: discGiB(wroteBytes - capacity, 2),
            })
          );
        }
        // The image, if one was asked for. After the index and not instead
        // of it: the image is made of the folder, which has to be finished
        // before there is anything to wrap.
        if (settings.image) {
          try {
            // Asked for after the rows were made, if the setting was turned
            // on while the index was being written: a row it never got is
            // not worth failing the image over.
            if (discStep("image")) discStep("image").state = "running";
            phaseBegan = Date.now();
            paintOutProgress(0, phaseBegan);
            el("out-state").textContent = t("out.imaging", { udf: settings.image, pct: 0 });
            renderOutScreen();
            const path = await invoke("bdav_image", {
              dir: discDir(),
              title: discTitleFor(list),
              revision: settings.image,
              access: settings.imageAccess,
            });
            finishStep("image", "done");
            note(t("out.imageDone", { path }));
            // And the folder, if that was asked for. After the image and
            // never instead of it -- the image is made of the folder, so a
            // folder taken away before there is an image is the disc lost --
            // and a removal that fails is a note rather than a failed step:
            // the image it was to make room for is written and whole.
            if (settings.imageOnly) {
              try {
                note(t("out.folderGone", { path: await invoke("bdav_drop", { dir: discDir() }) }));
              } catch (e) {
                note(t("out.folderStays", { e: String(e) }));
              }
            }
          } catch (e) {
            finishStep("image", "error");
            note(t("out.imageFailed", { e: String(e) }));
          }
        }
      } catch (e) {
        finishStep("index", "error");
        note(t("out.bdavFailed", { e: String(e) }));
      }
    }
  }

  exporting = false;
  runDir = null;
  runFolder = null;
  // The answer about the folder is left standing rather than thrown away.
  // The folder this run made is now a folder that is there, so asking again
  // here would answer with the branch the *next* run would be given -- and
  // the path on screen would turn into `night-2` the moment a run that
  // wrote into `night` finished, which reads as the output having gone
  // somewhere other than where it was being watched. So the screen goes on
  // naming the folder this run wrote. The next run puts the question again
  // itself, with `force`, and says out loud where the name moved to; and a
  // hand on the name or on the folder above asks again as it is typed. See
  // `askFreeFolder`.
  writing = null;
  paintExportButton();
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
  // Whatever is on the stage now is the last frame the run made: hold it.
  heldAfterRun = !!onShow;
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

/// Whether a name would be opened as a protocol rather than as a file.
///
/// The engine hands a name it does not recognise straight to libavformat,
/// which opens a good deal more than files -- that is deliberate, and on the
/// command line it is the point. The window is another matter: its names come
/// from the picker, from a drop and from the command line, and a project file
/// is the one that can have been written by somebody else. A row naming
/// `http://…` would be fetched the moment the project was opened, because the
/// index lanes start on their own as soon as the list is up.
///
/// A drive letter is not a protocol, and neither is the one share spelling
/// the window resolves to a mount point itself.
function namesAProtocol(path) {
  return (
    /^[A-Za-z][A-Za-z0-9+.-]*:/.test(path) &&
    !/^[A-Za-z]:[\\/]/.test(path) &&
    !/^smb:/i.test(path)
  );
}

/// The format's own number, which is not the program's. It goes up when a
/// file written by an older version would be read *wrongly* rather than
/// merely incompletely -- a field added is not a new format, since a reader
/// that has never heard of it leaves it alone.
const PROJECT_VERSION = 1;

/// The file this list is currently in, or "" for work that has never been
/// saved. What 保存 writes over without asking.
let projectPath = "";

/// The copy of this list that the queue was given, for a list that had no
/// file of its own to give it -- see `projectForQueue`. Not the project this
/// window is about, and never shown as one: it is a file in the queue's own
/// folder that the queue will delete with the job. Held so that registering
/// the same list twice writes the one copy rather than a second.
///
/// Goes with the list it is a copy of: another project opened, or a new one
/// started, and this one is nobody's copy.
let tempProject = "";

/// The queued job this window was opened on, or "" for a window that is
/// nobody's job. Settled once at startup; see `queued_job`.
///
/// What it changes is one button: a window opened out of the queue is there
/// to put something back into it, so バッチに登録 reads バッチを上書き and
/// writes this file rather than making the queue a copy of its own.
let queuedJob = "";

/// Whether the output settings are this list's answer, or merely what the
/// program happens to be holding.
///
/// A list saved from the 入力 screen has been given no output. What
/// `settings` holds at that moment is the program's own defaults, 環境設定's
/// standing answer for what a cut is called, and whatever the last session
/// was carrying -- none of which anybody has said about *this* work. Written
/// down they would stop being standing answers and become the project's own,
/// and the file would go on answering with them long after 環境設定 had been
/// told otherwise.
///
/// So they go in only once somebody has settled them, which is the moment a
/// control on the output settings screen is used. Until then the file says
/// nothing about the output at all, and opening it asks the standing answer
/// again -- the same three things 新規作成 puts back.
///
/// Being drawn is not settling. The output settings screen fills in a folder
/// name and a disc title by itself, from the recordings; those are what it
/// *would* write, worked out again as readily next time, and nobody has
/// chosen them by walking past them.
let outputSettled = false;

/// Somebody has just answered for this project's output.
function settleOutput() {
  outputSettled = true;
}

/// Everything worth keeping, in the shape it goes on disc.
///
/// `settled` is whether the output settings are part of it, which for a
/// project somebody is saving is `outputSettled`. A queue copy passes `true`
/// whatever this window has been shown: the job is to be written the way the
/// window would write it now, and one that worked its output out again in
/// another process, hours later and from that process's preferences, would
/// not be the run that was asked for.
///
/// `forRun` says which of those two this is, for the one setting that is not
/// an answer until somebody makes it one. The folder of its own is filled in
/// from the project's name -- see `filledIn` -- and a name still holding
/// that fill is written down as "nobody has said", so that the file opened
/// tomorrow under another name is written into a folder of *that* name
/// rather than going on naming the project it was copied from. The queue
/// copy writes the name itself: its job is to run the way this window would
/// run it now, and the copy is a file in the queue's own folder whose name
/// nothing should be called after.
function captureProject(settled = outputSettled, forRun = false) {
  const kept = { ...settings };
  if (!forRun && kept.subfolder === filledIn) kept.subfolder = null;
  // Written down as a position rather than as the row id it is held as: an
  // id is this session's counting and means nothing in the next one. Null
  // where nobody has chosen, which is the first row either way.
  const at = clips.findIndex((c) => c.id === settings.master);
  kept.master = at < 0 ? null : at;
  return {
    smartcut: PROJECT_VERSION,
    saved: stampISO(),
    // Left out altogether rather than written empty: a reader has to be able
    // to tell "nobody has said" from "somebody said none of it".
    settings: settled ? kept : undefined,
    clips: clips.map((c) => ({
      path: c.path,
      // What somebody renamed the row to. The one name in the list that
      // nothing can work out again from the recording.
      renamed: c.renamed || undefined,
      // What a disc's index said about it. Written down because reopening the
      // project must not have to read the disc again -- it may not be in the
      // drive -- and because a row that came back called `00001.m2ts` would
      // not be the row that was saved.
      name: c.stem ? c.name : undefined,
      stem: c.stem || undefined,
      home: c.home || undefined,
      // Written for the same reason as the name: reopening the project must
      // not need the disc back in the drive to know where the chapters were.
      chapters: c.chapters.length ? c.chapters : undefined,
      // And for the same reason again: the tracks switched off when the disc
      // was read are an answer given to a question the disc asked, and
      // asking it again would mean reading the disc again.
      dropPids: c.dropPids.length ? c.dropPids : undefined,
      // What a disc this program writes will say about the recording: what
      // the disc it came off said about it, and the name somebody typed over
      // the one it arrived with.
      // `??` and not `||`: an emptied field is an answer -- write nothing
      // there -- and a project that read it back as "nobody has said" would
      // fill it in again from the recording.
      made: c.made ?? undefined,
      description: c.description ?? undefined,
      channel: c.channel ?? undefined,
      channelNumber: c.channelNumber ?? undefined,
      programme: c.programme || undefined,
      // What the editor handed back the last time this row was in it: the
      // cuts, the marks, and where the playhead was left. Null for a row
      // nobody has opened yet, which is not the same as a row cut to nothing.
      edit: c.edit,
      // ...and whether any of it was somebody's doing rather than a
      // detection's. Left out on a row nobody has been through, which is what
      // a file written before this existed looks like. See `edited`.
      edited: c.edited || undefined,
      // What happens where this row gives way to the next, when the list is
      // being written as one file. Left out where nobody has said, which is
      // every row of every list that is not being joined.
      after: crossingSet(c.after) ? c.after : undefined,
      // Blocks a detection found that the timeline has not been shown yet.
      // The blocks are not written -- they are beside the recording -- but
      // whether they are still owed to the editor is this list's own
      // knowledge, and the cache cannot answer it.
      cmPending: c.cmPending,
    })),
  };
}

/// Where a list that has no name yet belongs: beside the output if one has
/// been chosen, and otherwise beside the recordings, because that is where
/// the work is. With its separator, so a name can be put straight after it.
function projectHome() {
  const first = clips[0];
  const beside = first && first.home
    ? `${first.home.replace(/[/\\]*$/, "")}/`
    : dirOf(first ? first.path : "");
  return settings.dir ? settings.dir.replace(/[/\\]*$/, "/") : beside;
}

/// Where the picker opens for such a list.
function defaultProjectPath() {
  return `${projectHome()}${t("project.untitled")}.${PROJECT_EXT}`;
}

/// And what it is called where nobody is asked -- see `enlistExport`.
///
/// The disc's name where a disc is being built, and otherwise the first
/// recording's, which is what the row at the top of the list says. A queue of
/// rows called 無題, 無題-2, 無題-3 would be a queue nobody could read, and
/// the name of the first recording is the one thing about a list that is
/// already on screen when the button is pressed.
function autoProjectStem() {
  const list = ready().length ? ready() : clips;
  const name = bdavMode() ? discTitleFor(list) : list[0] ? outStem(list[0]) : "";
  return filenameSafe(name) || t("project.untitled");
}

/// What the project would be if it were written this instant, as one string.
///
/// The saved timestamp is left out -- it changes every time and says nothing
/// about the work -- and so is `cmPending`, which is written to the file but
/// is not something that can be *lost*: a detection is cached beside the
/// recording, and opening the project again works the flag out from it.
function shapeOf() {
  return JSON.stringify({
    // The same question the file answers: an output nobody has settled is
    // not written, so it is not something the file can be behind on either.
    // Without this, walking onto the output settings screen -- which fills
    // the folder name in by itself -- would put a `*` in the title over a
    // change no save would record and no save could clear.
    settings: outputSettled ? settings : null,
    clips: clips.map((c) => ({
      path: c.path,
      renamed: c.renamed,
      dropPids: c.dropPids,
      programme: c.programme,
      edit: c.edit,
      edited: c.edited,
      after: c.after,
    })),
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
  // The batch tool is named for what it is, whatever project it happens to
  // have open: the list it is holding is one job of a queue, not the work.
  // Nor is there unsaved work in it to stop it closing -- a job it did not
  // finish is a job still in the queue.
  if (isTool()) {
    if (shownTitle !== t("batch.windowTitle")) {
      shownTitle = t("batch.windowTitle");
      invoke("retitle_main", { title: shownTitle });
    }
    return;
  }
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

/// Put the list down as a file, and say nothing about it.
///
/// The writing half of 保存, without the part that says this is now the
/// project the window is about -- which is not true of the copy the queue is
/// given; see `projectForQueue`. That copy is also the one caller that asks
/// for the output settings whether or not they have been settled.
async function putProject(path, settled = outputSettled, forRun = false) {
  try {
    // Indented, and with the paths first in every row: a project is a plain
    // file about files, and someone who opens one in an editor to see which
    // recordings it names should be able to read it.
    await invoke("write_project", {
      path,
      body: JSON.stringify(captureProject(settled, forRun), null, 2),
    });
  } catch (e) {
    note(`${e}`);
    return false;
  }
  return true;
}

async function writeProject(path) {
  if (!(await putProject(path))) return false;
  projectPath = path;
  // The folder of its own is named after the project, and the project has
  // just been given a name -- its first, or another one under 名前を付けて
  // 保存. Settled here rather than at whatever redraw comes next, so that
  // the field agrees with the title bar from this moment on; and before the
  // shape is put down, because a name that moved afterwards would raise a
  // `*` over a change nobody made and no save could clear. See
  // `settleSubfolder`.
  renderOutset();
  renderOutScreen();
  savedShape = shapeOf();
  retitleMain();
  note(t("project.saved", { name: nameOf(path) }));
  // 保存 over the job this window was opened on is バッチを上書き by another
  // name, and the tool has to hear about it either way.
  await touchQueuedJob(path);
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
///
/// The wording is the caller's because the two things that put a list down
/// are not the same question: one is replacing this work with another
/// project's, the other is throwing it away for an empty list.
async function askReplace(title = t("project.replaceTitle"), body = t("project.replaceBody")) {
  if (!dirty()) return true;
  return dialog.ask(body, { title, kind: "warning" });
}

/// An empty list with nothing behind it: the state the program opens in,
/// reached without closing it.
///
/// What `loadProject` does, with nothing to put back afterwards: the list
/// emptied properly rather than merely dropped, the output settings back
/// where they started, and no file behind the work -- so that the first 保存
/// asks for a name instead of writing over the project this one was started
/// from.
async function newProject() {
  if (!(await askReplace(t("project.newTitle"), t("project.newBody")))) return;
  await remove(clips.slice());
  // Back to the defaults -- unless the settings are being carried, in which
  // case the answer to "what should a new list be written as" is the one
  // being carried. That is what the preference is for: a new project then
  // starts where the last one left off rather than at the program's idea of
  // a first run.
  for (const key of Object.keys(SETTING_DEFAULTS)) settings[key] = SETTING_DEFAULTS[key];
  applyNameDefaults();
  restoreOutput();
  // And nobody has answered for this list's output, whatever is in force.
  outputSettled = false;
  filledIn = null;
  showSettings();
  projectPath = "";
  tempProject = "";
  // An empty list is nobody's job, whatever this window was opened on.
  queuedJob = "";
  savedShape = shapeOf();
  retitleMain();
  show("input");
  renderList();
  note(t("project.newDone"));
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

/// Put a project file's list up, in place of whatever is there. `false` where
/// the file could not be read, or is from a later version of the program.
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
    return false;
  }
  // A number this program has never heard of is a file from a later one, and
  // what it would lose on the way in is exactly the part it does not
  // recognise. Dropping somebody's cuts quietly is worse than not opening.
  if (!doc || typeof doc.smartcut !== "number" || doc.smartcut > PROJECT_VERSION) {
    note(t("project.wrongFormat", { name: nameOf(path) }));
    return false;
  }
  // Not merely emptied: a lane reading a row has to be told to stop, and the
  // editor open on one has nothing left to be about. All of which `remove`
  // already knows how to do.
  await remove(clips.slice());
  // Whether this project has an output of its own. One saved from the 入力
  // screen has not been given one and says nothing about it; see
  // `outputSettled`.
  const said = doc.settings && typeof doc.settings === "object" ? doc.settings : null;
  outputSettled = !!said;
  // Which row the file says the joined output takes its shape from. See
  // `captureProject`, which writes it down as a position.
  let masterAt = null;
  // **Whatever the file does not answer for is the standing answer, never the
  // last project's.** Put back before the file is read rather than only where
  // there is nothing to read: a project holds an output settled by a version
  // of this program that had fewer things to settle -- or was written by
  // hand -- and every key it is silent about would otherwise keep whatever
  // the list opened before it was carrying. In the batch tool, which opens
  // one project after another in the same window with nobody watching, that
  // is a job that writes a BDAV disc because the job in front of it did,
  // under the disc title that job was given.
  //
  // 環境設定's names go on top, because those three are as much about the
  // person as about the work: somebody who writes `編集_` in front of every
  // file writes it in front of a project that never said.
  for (const key of Object.keys(SETTING_DEFAULTS)) settings[key] = SETTING_DEFAULTS[key];
  applyNameDefaults();
  if (said) {
    // Key by key rather than wholesale, so that a file cannot put anything in
    // `settings` that the output screen has no control for. What it leaves
    // out keeps the answer put back above -- which for `subfolder` is null,
    // so a project written before there were folders of their own opens
    // unsettled and the name follows the project just opened.
    for (const key of Object.keys(settings)) {
      if (key in said) settings[key] = said[key];
    }
    // Held as a position in the file and as a row id here; the rows do not
    // exist yet, so the position is kept and turned into an id below.
    masterAt = Number.isInteger(said.master) ? said.master : null;
    settings.master = null;
  } else {
    // Nothing to put back, so what this list is written with is the standing
    // answer -- the three above, and whatever is being carried from the last
    // session. What 新規作成 puts back, for the same reason: the project that
    // was open a moment ago has nothing to say about this one.
    restoreOutput();
  }
  // Whether the folder name that came in is this program's own fill or
  // somebody's answer, which is what decides whether it goes on following
  // the project. The file does not say in so many words -- and a build
  // before this one wrote the fill down as though it were an answer -- so a
  // name that is the project's own, or the disc's, is taken to be one of
  // those: `foo.scproj` saved again as `bar.scproj` writes into `bar`, and a
  // name somebody typed is left exactly as they typed it. See `filledIn`.
  const ours = filenameSafe((bdavMode() ? settings.discTitle : stemOf(path)) || "");
  filledIn = ours && settings.subfolder === ours ? ours : null;
  showSettings();
  const taken = [];
  let refused = 0;
  for (const saved of Array.isArray(doc.clips) ? doc.clips : []) {
    if (!saved || typeof saved.path !== "string") continue;
    // A project is the one thing in the list that can arrive from somebody
    // else, and every row in it is opened without being asked for -- the
    // index lanes start as soon as the list is up. See `namesAProtocol`.
    if (namesAProtocol(saved.path)) {
      refused += 1;
      continue;
    }
    const clip = makeClip(saved);
    // The row's id is this session's counting, so the saved edit is
    // readdressed to the row it has just become. Everything else in it is
    // source time and travels unchanged.
    //
    // The two lists an edit is read for are filled in where the file leaves
    // them out. A project is the one thing in the list that can arrive from
    // somebody else -- from an older version of this program, or from a hand
    // that wrote it -- and everything downstream asks an edit for its cuts
    // and its keyframes without asking first whether it has any. One missing
    // field would otherwise be a row that cannot be drawn at all, which in
    // the batch tool is a job that cannot be run and a queue that stops on
    // it.
    if (saved.edit) {
      clip.edit = {
        ...saved.edit,
        cuts: Array.isArray(saved.edit.cuts) ? saved.edit.cuts : [],
        keyframes: Array.isArray(saved.edit.keyframes) ? saved.edit.keyframes : [],
        id: clip.id,
        path: clip.path,
      };
    }
    clips.push(clip);
    taken.push([clip, !!saved.cmPending]);
  }
  // The rows exist now, so the position the file gave can become the row it
  // names. A list shorter than the file said -- a recording that has moved
  // away -- falls through to the first row, which is what no answer means.
  settings.master = masterAt !== null && clips[masterAt] ? clips[masterAt].id : null;
  projectPath = path;
  tempProject = "";
  // A window opened on a job that has since been pointed at another project
  // is not holding that job any more, and バッチを上書き would write this
  // list into a file it is no longer about. Unless it is the same file --
  // which is how the window opens in the first place.
  if (path !== queuedJob) queuedJob = "";
  savedShape = shapeOf();
  retitleMain();
  // Onto the screen the list is on -- except in the batch tool, which has no
  // such screen on its bar and is never looking at anything but its queue.
  if (!isTool()) show("input");
  renderList();
  note(
    refused
      ? t("project.refused", { name: nameOf(path), n: taken.length, bad: refused })
      : t("project.opened", { name: nameOf(path), n: taken.length })
  );
  (async () => {
    for (const [clip, pending] of taken) {
      await restoreCm(clip, pending);
      await restoreFlat(clip);
    }
  })();
  fillFirstLook();
  pump();
  // Whether the list is up, for the one caller that has something to do
  // about it: バッチ出力 runs the job after this one rather than writing out
  // whatever was on screen when the file turned out to be missing.
  return true;
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

// The four items the menu carries about the work rather than about the
// program. `showMenu(false)` first in each: the file picker is a window of
// its own, and a menu left standing behind it is still there when it closes.
el("menu-new").addEventListener("click", () => {
  showMenu(false);
  newProject();
});
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

// Ctrl+N, Ctrl+S, Ctrl+Shift+S and Ctrl+O, on every screen rather than only
// on the list: they are about the program's work as a whole, and the output
// settings are as much a part of a project as the cuts are. Kept out of the
// list's own key handler for that reason.
window.addEventListener("keydown", (ev) => {
  if (!(ev.ctrlKey || ev.metaKey) || ev.altKey) return;
  // Not in the batch tool, for the reason the items are off its menu: what it
  // has open is a job out of the queue, and Ctrl+S over that file is a queue
  // editing its own work.
  if (isTool()) return;
  const key = ev.key.toLowerCase();
  if (key === "s") {
    ev.preventDefault();
    saveProject(ev.shiftKey);
  } else if (key === "o" && !ev.shiftKey) {
    ev.preventDefault();
    openProject();
  } else if (key === "n" && !ev.shiftKey) {
    // Taken off the webview, which would otherwise answer it with a browser
    // window of its own.
    ev.preventDefault();
    newProject();
  }
});

// --- バッチ出力 -----------------------------------------------------------
//
// A queue of saved projects, written out one after another without anybody in
// the room. The 出力 screen already writes a whole list in one pass; what this
// adds is the other axis -- six lists, settled on six different evenings, each
// with its own output folder and its own idea of what the disc is called.
//
// **A job is a `.scproj` and nothing else.** A project already holds the
// recordings, the cuts and the output settings, which is the whole of what a
// job is, so a queue that held a second copy of any of it would be a queue
// that could disagree with the file. What the queue keeps is the path, a name
// to show it under, and whether it has been written yet.
//
// **The queue belongs to a program of its own** -- this same executable
// started with `--batch`, which opens this screen and the 出力 screen and
// nothing else. That is the point of it: a queue lined up at midnight has to
// go on being written when the window it was lined up in is closed. See
// `open_batch_tool`.
//
// So the list window never shows this screen. What it does with the queue is
// put a list into it -- `enlistList`, from the button under 出力開始 -- and
// open the tool, from the menu. An append is a write that cannot disturb the
// row the tool is working on, so it needs no turn-taking with it; everything
// else about the queue is the tool's, which is why it is all on this screen.
//
// Running a job is exactly what a person does by hand: open the project, wait
// for the list to be read, press 出力開始. So that is what this does -- the
// same `loadProject` and the same `runExport`, in a loop -- rather than a
// second engine that would have to be kept in step with the first. A job gets
// the disc pass, the image, the sidecars and the failures of the screen it is
// driving, because it *is* the screen it is driving.

/// Which window this is: "main" for the list window, "batch" for the tool.
let batchRole = "main";
/// The jobs, in the order they will run.
let batchJobs = [];
/// Which rows the ↑↓ and 削除 buttons are about, by path. A set rather than
/// one path, and picked the way the clip list picks rows: a queue lined up
/// for the night is a queue whose middle five rows are sometimes all wrong,
/// and taking them out one at a time is the work this saves.
let batchPicked = new Set();
/// Where a shift-range is measured from: an index into `batchJobs`, or -1.
/// The clip list's `anchor`, and for the same reason.
let batchAnchor = -1;
/// What to do when the queue empties. The tool's answer; the list window
/// shows the queue but never runs it, so it never acts on this.
let batchAfter = "nothing";
let batchRunning = false;
/// Set by すべて中止, read between jobs and in the wait for the list.
let batchStopped = false;
/// The countdown to sleeping or shutting down, while there is one.
let afterTimer = null;
/// The button is down on a row of the queue, and it is not (yet) a drag.
let jobPress = null;
/// The drag proper: which rows are being carried, and where they would land.
let jobDrag = null;

const isTool = () => batchRole === "batch";

/// The picked rows, in queue order.
const pickedJobs = () => batchJobs.filter((j) => batchPicked.has(j.path));
/// The one picked row, for the things that are about a file rather than about
/// a stretch of the queue -- opening the project, opening the folder it writes
/// into. Null where five rows are picked, the way the clip list's カット編集
/// goes out when five clips are.
const onePicked = () => (batchPicked.size === 1 ? pickedJobs()[0] || null : null);
/// The ends of the picked stretch, for the four that move it.
const firstPicked = () => batchJobs.findIndex((j) => batchPicked.has(j.path));
const lastPicked = () => {
  for (let i = batchJobs.length - 1; i >= 0; i -= 1) {
    if (batchPicked.has(batchJobs[i].path)) return i;
  }
  return -1;
};

/// Say something where whoever asked for it is looking.
///
/// The note line the rest of this window uses is on the input screen, and
/// somebody who pressed a button on the output screen is not looking at it.
const sayHere = (text) =>
  screen === "out" ? (el("out-state").textContent = text) : note(text);

/// Put the queue down whole. Only ever called where this window owns it: the
/// tool always, and the list window while no tool is running.
async function saveQueue() {
  if (!invoke) return;
  try {
    await invoke("batch_write", {
      queue: {
        jobs: batchJobs.map((j) => ({
          path: j.path,
          label: j.label,
          // Not this side's to set -- the window that wrote the project sets
          // it -- but this side's to carry, or saving the queue would tell
          // the next poll that every project had gone back to its first
          // version. See `batch_touch`.
          edited: j.edited || 0,
          // 待機 for a job called off in this run: calling one off is about
          // the run rather than about the job, and the queue reopened
          // tomorrow is a queue of work still to do.
          //
          // **中 stays 中.** It is what the other window reads to see that
          // the tool is working, and it is the one mark left on the file by a
          // tool that went away with a job open -- which is what
          // `takeBackInterrupted` mends. A row written 待機 there would be a
          // row nothing could tell from one nobody had started.
          state: ["done", "error", "running"].includes(j.state) ? j.state : "waiting",
          note: j.note || "",
        })),
        after: batchAfter,
      },
    });
  } catch (e) {
    note(t("batch.queueFailed", { e: String(e) }));
  }
}

/// Read the queue back off the file.
///
/// On a timer in both windows: it is how the list window watches the tool get
/// on with it, and how the tool notices a job added while it runs. The row
/// somebody had picked is kept by path rather than by place, because the
/// place can have moved.
async function refreshQueue() {
  if (!invoke) return;
  let queue;
  try {
    queue = await invoke("batch_read");
  } catch {
    return;
  }
  // What the file holds, plus what this session has watched happen to it: how
  // far a job got and how long it took are not in the file -- they are about
  // a run rather than about a job -- and a poll that dropped them would empty
  // the bars two seconds after the queue finished.
  const watched = new Map(batchJobs.map((j) => [j.path, j]));
  batchJobs = (queue && Array.isArray(queue.jobs) ? queue.jobs : [])
    .filter((j) => j && typeof j.path === "string" && j.path)
    .map((j) => {
      const was = watched.get(j.path) || {};
      // The project behind this row has been written over since it was last
      // looked at, so what was kept about it -- the count, the picture, where
      // it writes -- is a version out of date. See `batch_touch`.
      if (was.path && (was.edited || 0) !== (j.edited || 0)) jobLook.delete(j.path);
      return {
        path: j.path,
        label: j.label || stemOf(j.path),
        state: j.state || "waiting",
        note: j.note || "",
        edited: j.edited || 0,
        began: was.began,
        spent: was.spent,
        done: was.done,
        since: was.since,
        // The frame the job was last seen on and the recording it came out
        // of: watched rather than written down, like the two above, and a
        // poll that dropped them would take the picture off the card a
        // second after the queue finished with it.
        shot: was.shot,
        now: was.now,
      };
    });
  batchAfter = queue && queue.after ? queue.after : "nothing";
  // A row that has left the queue -- written out of it by the tool, taken out
  // of it in the other window -- takes its pick with it.
  for (const path of [...batchPicked]) {
    if (!batchJobs.some((j) => j.path === path)) batchPicked.delete(path);
  }
  if (isTool()) paintAfterMenu();
  renderBatch();
  // What the rows show about each project, for any job not looked at yet.
  // Not awaited: the list is already up, and the rows fill in behind it.
  if (isTool()) lookAtJobs();
}

/// Take back a job left saying it was running, which is a job whose tool went
/// away under it -- closed with the cross, or killed, part way through.
///
/// Only the tool calls this, and only as it opens. Nothing else could leave
/// such a row: there is never a second tool over the queue (see `batch_live`),
/// so a row that says it is running when this window opens is running nowhere.
///
/// Back to 待機 rather than to 失敗, because nothing about the job failed --
/// it is where 出力開始 would put it anyway, which is what makes the queue
/// pick it up again. The note is the one thing left to say, and it is gone
/// the moment the queue is started.
async function takeBackInterrupted() {
  let any = false;
  for (const job of batchJobs) {
    if (job.state !== "running") continue;
    job.state = "waiting";
    job.note = t("batch.interrupted");
    job.began = undefined;
    job.done = undefined;
    any = true;
  }
  if (!any) return;
  renderBatch();
  await saveQueue();
}

/// The tool's timer. While it is running a job the queue in hand is the truth
/// and the file is a copy of it, so reading it back would put a row the loop
/// has just moved on from back on the screen.
async function watchQueue() {
  if (batchRunning) return;
  await refreshQueue();
}

/// Whether this job is one a row's own 中止 can still be about: the one being
/// written, or one whose turn has not come. A queue that is not running has
/// nothing to call off, and a job that is finished -- written, failed or
/// already called off -- is past being stopped.
const canStop = (job) => batchRunning && (job.state === "running" || job.state === "waiting");

/// And whether もう一度出力する has anything to put back on it: a job that is
/// finished with this run. The one being written is not finished with it, and
/// one still waiting was never taken out.
const canRequeue = (job) => job.state !== "waiting" && job.state !== "running";

/// What a project says about itself, for the rows to show: where it writes,
/// how many recordings it holds, and a picture off the first of them.
///
/// Read once per job per session and kept here rather than in the queue file,
/// which holds what a job *is*. This is what it currently looks like, and a
/// project edited between two runs should look like what it now says.
const jobLook = new Map();

/// Fill that in for any job that has not been looked at yet.
///
/// One at a time, and after the list is already on screen: a picture costs a
/// seek and a GOP -- see `clip_glance` -- and twenty of them at once would be
/// twenty recordings opened before the window had drawn anything. The list is
/// repainted as each answer lands, so the rows fill in the way the clip list's
/// own do.
async function lookAtJobs() {
  for (const job of batchJobs) {
    if (jobLook.has(job.path)) continue;
    // Claimed before the first await, or the next pass would read it again.
    jobLook.set(job.path, {});
    let doc = null;
    try {
      doc = JSON.parse(await invoke("read_project", { path: job.path }));
    } catch {
      doc = null;
    }
    const settings = (doc && doc.settings) || {};
    const held = doc && Array.isArray(doc.clips) ? doc.clips : [];
    // Where a project with no folder of its own writes: beside each recording,
    // which is `home` for one read off a disc and the recording's own folder
    // otherwise -- the same answer `outputBase` arrives at. Kept as the
    // distinct ones, because a list drawn from three folders has three.
    const beside = [];
    for (const c of held) {
      if (!c || typeof c.path !== "string") continue;
      const at = c.home ? c.home.replace(/[/\\]+$/, "") : dirOf(c.path).replace(/[/\\]+$/, "");
      if (at && !beside.includes(at)) beside.push(at);
    }
    // The recording the row leads with, named the way the list window would
    // name that row: what somebody typed over it, else what the disc it came
    // off called it, else the file. See `makeClip`.
    const lead = held.find((c) => c && typeof c.path === "string" && c.path);
    const look = {
      dir: settings.dir || "",
      beside,
      // "" is a folder somebody emptied on purpose, which is no folder at
      // all. One nobody has settled is one the run will name itself -- after
      // the disc, or after the project file -- so the card names it the same
      // way, rather than showing a job that writes loose into the folder
      // above and then making a folder anyway. See `settings.subfolder` and
      // `autoSubfolder`.
      sub:
        settings.subfolder ??
        (settings.mode === "bdav"
          ? filenameSafe(settings.discTitle || "")
          : held.length > 1
            ? filenameSafe(stemOf(job.path))
            : ""),
      disc: settings.mode === "bdav",
      image: settings.image || "",
      clips: held.length,
      lead: lead ? lead.renamed || lead.name || nameOf(lead.path) : "",
      poster: "",
    };
    jobLook.set(job.path, look);
    renderBatch();
    const first = held.length && typeof held[0].path === "string" ? held[0].path : "";
    if (!first) continue;
    try {
      look.poster = (await invoke("clip_glance", { path: first })) || "";
    } catch {
      look.poster = "";
    }
    renderBatch();
  }
}

/// Where the job writes, which is the line a card leads with.
///
/// The folder the project names, and the folder of its own under that where
/// it has settled one. A disc goes into `BDAV` beneath it, said the way the
/// output settings screen says it. A project that writes beside its
/// recordings names no folder at all, and there is no one folder to name for
/// it: three recordings off three disks are three answers.
function jobPath(job) {
  const look = jobLook.get(job.path) || {};
  const beside = look.beside || [];
  // A project with no folder of its own writes beside its recordings, so that
  // is the folder to name -- and where the recordings came from more than one
  // folder, the first of them with a word for the rest. There is no one
  // answer there, and `/somewhere` alone would be the wrong half of it.
  const root = look.dir || beside[0] || "";
  if (!root) return t("outset.sameAsInput");
  const at = look.sub ? `${root.replace(/[/\\]+$/, "")}/${look.sub}` : root;
  if (look.disc) return t("outset.discPath", { dir: at });
  return look.dir || beside.length < 2 ? at : t("batch.andElsewhere", { dir: at, n: beside.length - 1 });
}

/// The line under it: the recordings, and what is to become of them.
///
/// **Recordings, whatever the job is doing.** While it runs, the one the pass
/// has open -- a queue at work is watched to see how far it has got, and the
/// recording being read is the answer. Before and after, the first of them,
/// with a word for how many more there are.
///
/// The project's own file is not named here, and this is the one place it
/// could have been. A job runs a copy the queue made for itself, in a folder
/// of the queue's own (see `queue_copy`), so the name on that file is a fact
/// about this program's scratch space rather than about the evening's work --
/// and it would be the one thing on the card that was neither a recording nor
/// a folder anybody chose. What the job was registered under stands in for a
/// project that cannot be read to say anything else, which is a name somebody
/// gave it rather than a file.
///
/// See `followJob` for where the running name comes from and when it goes.
function jobLine(job) {
  const look = jobLook.get(job.path) || {};
  const bits = [];
  if (job.state === "running" && job.now) {
    // The count stays beside it: the name no longer says which recording of
    // the job this is, and how many there are is how big the job is. Not for
    // a job of one, where it says nothing the name has not -- the same reason
    // the resting line folds the count into "ほか n 本" and leaves it out
    // where there is no other recording to be among.
    bits.push(job.now);
    if (look.clips > 1) bits.push(t("batch.clips", { n: look.clips }));
  } else if (look.lead) {
    bits.push(
      look.clips > 1 ? t("batch.andMore", { name: look.lead, n: look.clips - 1 }) : look.lead
    );
  } else {
    bits.push(job.label);
  }
  if (look.disc) bits.push(look.image ? t("batch.toImage", { udf: look.image }) : t("batch.toDisc"));
  return bits.join(t("sep"));
}

/// How far a job has got, as the bar and the percentage both read it.
///
/// A written job is full whatever else is known about it -- a queue reopened
/// tomorrow has only the states, and a job that reached the end should look
/// as though it did. Everything else is as far as it actually got, which for
/// one called off part way through is where it stopped.
const jobHow = (job) => (job.state === "done" ? 1 : job.done || 0);

/// What stands in the bar: the numbers.
///
/// Everything a row has to say in words -- what is being written, or that it
/// is waiting, written, failed or called off -- is said on the line above, at
/// the right. The bar is left the three numbers, one at each end and one in
/// the middle, so that none of them is read past to find another.
///
/// The percentage is always there, so that the box is always a line high and
/// the bars down the list are all the same bar. The two clocks appear once the
/// job has actually run -- a queue reopened tomorrow cannot say how long
/// yesterday's jobs took -- and stay once it has stopped: what is left of a
/// job that has ended is none of it, which is worth saying rather than
/// leaving the row to be read with a gap where the answer was.
function jobSaid(job) {
  const how = jobHow(job);
  const spent = job.spent ?? (job.began ? (Date.now() - job.began) / 1000 : null);
  // Time gone is the whole job; time left is worked out inside the stretch
  // the bar is currently about, which for a disc is the index or the image
  // rather than the job. The output screen's clock is read the same way and
  // for the same reason: the rate of the cuts says nothing about the rate of
  // the pass that wraps them. See `paintOutProgress`.
  const inHand = job.state === "running" ? (Date.now() - (job.since || job.began)) / 1000 : spent;
  const left =
    spent === null
      ? null
      : job.state !== "running"
        ? 0
        : how > 0.01
          ? (inHand / how) * (1 - how)
          : null;
  return `<span class="ela">${
    spent === null ? "" : esc(t("batch.elapsed", { t: clock(spent) }))
  }</span>
      <span class="grow"></span>
      <span class="pct">${Math.round(how * 100)}%</span>
      <span class="grow"></span>
      <span class="rest">${left === null ? "" : esc(t("batch.left", { t: clock(left) }))}</span>`;
}

function renderBatch() {
  // Not while a row is being carried: the list is rebuilt whole, and rebuilt
  // rows are not the ones the drag is holding on to. The poll that would have
  // repainted it comes round again the moment the row lands.
  if (jobDrag) return;
  const count = (state) => batchJobs.filter((j) => j.state === state).length;
  el("batch-total").textContent = t("batch.counts", {
    all: batchJobs.length,
    run: count("running"),
    wait: count("waiting"),
    done: count("done"),
    bad: count("error"),
    off: count("skipped"),
  });
  // Where the queue is scrolled to, kept over the rebuild below: this runs
  // off the poll as well as off anything anybody does, and a long queue that
  // jumped back to the top twice a second would be one nobody could read the
  // bottom of.
  const listAt = el("batch-list").scrollTop;
  el("batch-list").innerHTML = batchJobs
    .map((j, i) => {
      const look = jobLook.get(j.path) || {};
      // The frame the run is on, where the run has reached this row; the
      // first recording's own picture until then. A picture either way, or
      // the hatching the clip list shows where there is not one yet -- the
      // same box in all three, so the rows line up while they fill in.
      const shot = j.shot || look.poster;
      const poster = shot
        ? `<img class="poster" src="${shot}" alt="" draggable="false">`
        : `<span class="poster blank"></span>`;
      return `<li class="${j.state}${batchPicked.has(j.path) ? " picked" : ""}" data-i="${i}">
        <span class="n">${i + 1}</span>
        ${poster}
        <div class="meta">
          <div class="nm">${esc(jobPath(j))}</div>
          <div class="sub">
            <span class="who dim">${esc(jobLine(j))}</span>
            <span class="doingnow ${j.state}">${esc(j.note || "")}</span>
          </div>
          <div class="say">
            <span class="fill"></span>
            <span class="what">${jobSaid(j)}</span>
          </div>
          <button class="jobstop mini" data-stop="${i}"${
            canStop(j) ? "" : " disabled"
          }>${esc(t("batch.stopJob"))}</button>
        </div>
        ${
          batchRunning
            ? ""
            : `<button class="kill" data-kill="${i}" title="${esc(t("batch.drop"))}">×</button>`
        }
      </li>`;
    })
    .join("");
  // The fill behind each sentence is set from here rather than written into
  // the markup: a `style` attribute in markup is the one thing the window's
  // content policy turns off, and the property is not.
  batchJobs.forEach((j, i) => {
    const fill = el("batch-list").children[i]?.querySelector(".say .fill");
    if (fill) fill.style.width = `${Math.round(jobHow(j) * 100)}%`;
  });
  el("batch-list").scrollTop = listAt;
  paintBatchButtons();
}

/// The job this window is writing at this moment. Null between jobs, and in
/// the list window, where the queue is watched rather than run.
const runningJob = () =>
  batchRunning && exporting ? batchJobs.find((j) => j.state === "running") || null : null;

/// Carry what the 出力 screen is showing onto the card of the job it is
/// showing it for. True where any of it changed, which is when the card has
/// to be drawn again.
///
/// The tool drives that screen with nobody looking at it -- the queue is what
/// is on screen while it runs -- so everything the screen says about the job
/// in hand has to reach the one row that is about that job.
function followJob(job) {
  const was = [job.note, job.shot, job.now];
  // What the pass itself says it is writing, which is the sentence the output
  // screen would be showing. Said on the card rather than invented again
  // here: there is one true answer to "what is being written" and it is that
  // one -- and it is the answer for the passes that wrap a disc as much as
  // for the cuts.
  const said = el("out-state").textContent;
  if (said) job.note = said;
  // The frame on the stage, which through the cuts is the picture the encoder
  // is making. It is the one thing about the output this program is answerable
  // for -- every other frame is copied -- and there is no reason for somebody
  // watching a queue to see less of it than somebody watching one list. Held
  // on the row rather than read off the screen as the row is drawn, so that a
  // job that has run goes on showing the frame it ended on, the way the stage
  // itself holds that frame.
  if (stageSrc) job.shot = stageSrc;
  // And the recording that frame came out of. Left standing between two
  // recordings, because the gap is the width of one `await`; taken away when
  // the disc passes begin, because those read the written streams back rather
  // than anybody's recording, and a card still naming one would be naming a
  // file nothing is reading.
  // The row under the head rather than the row the reports are tagged with:
  // a join's reports all carry the first recording, and the head is in the
  // ninth of twelve. See `writingJoin`.
  if (writing) job.now = clipLabel(writingJoin && writingJoin.at ? writingJoin.at.clip : writing);
  else if (discSteps.some((s) => s.state === "running")) job.now = "";
  return job.note !== was[0] || job.shot !== was[1] || job.now !== was[2];
}

/// How far the run has got, onto the row whose job it is.
///
/// `since` is when the stretch that number is about began: the run itself
/// while the cuts are written, and each of the two passes a disc is finished
/// with while it is the one working. The bar starts again for each of them,
/// the way the output screen's own bar does -- a bar that sat full while the
/// image was still being written was saying the job was over.
function paintJobProgress(done, since) {
  const job = runningJob();
  if (!job) return;
  const moved = Math.round((job.done || 0) * 100) !== Math.round(done * 100);
  job.done = done;
  job.since = since;
  const changed = followJob(job);
  if (changed || moved) renderBatch();
}

function paintBatchButtons() {
  // A queue that is being written is not one to rearrange: the row order is
  // what the loop is walking. Adding is the exception, and is not on this
  // screen anyway -- see `batch_append`.
  const left = batchJobs.some((j) => j.state !== "done");
  el("batch-go").disabled = batchRunning || !left || exporting;
  // Never disabled while the queue runs: a stop has to be there the moment it
  // is wanted.
  el("batch-stop-all").disabled = !batchRunning;
  el("batch-add").disabled = batchRunning;
  el("batch-drop").disabled = batchRunning || !batchPicked.size;
  el("batch-more").disabled = batchRunning || !batchJobs.length;
  el("batch-clear-done").disabled = !batchJobs.some((j) => j.state === "done");
}

/// The little menu on 削除, which holds the two ways of doing it in bulk.
///
/// A `function` rather than a `const`, because `show` closes it and `show` is
/// declared a long way above this.
function showBatchMenu(on) {
  const menu = el("batch-menu");
  if (!menu) return;
  menu.hidden = !on;
  el("batch-more").setAttribute("aria-expanded", String(!!on));
  el("batch-more").classList.toggle("open", !!on);
}

/// Put a project in the queue, under the name its own file gives it.
///
/// The file is read here only to count the recordings: a row that said nothing
/// but a path would be a row nobody could tell from the one under it. Read
/// again when the job runs, so a project edited in between is written out as
/// it now stands.
async function addBatchJob(path, label = stemOf(path)) {
  if (batchJobs.some((j) => j.path === path)) {
    sayHere(t("batch.already", { name: label }));
    return false;
  }
  let clipsIn = 0;
  try {
    const doc = JSON.parse(await invoke("read_project", { path }));
    clipsIn = Array.isArray(doc.clips) ? doc.clips.length : 0;
  } catch (e) {
    sayHere(t("project.cannotOpen", { name: nameOf(path), e }));
    return false;
  }
  // Appended by the backend rather than written from here: the tool may be
  // partway down the queue, and everything it has written about the rows
  // above has to survive a job being added below them.
  try {
    await invoke("batch_append", {
      jobs: [{ path, label, state: "waiting", note: t("batch.clips", { n: clipsIn }) }],
    });
  } catch (e) {
    sayHere(t("batch.queueFailed", { e: String(e) }));
    return false;
  }
  await refreshQueue();
  return true;
}

const jobAt = (ev) => {
  const li = ev.target.closest("li[data-i]");
  return li ? batchJobs[Number(li.dataset.i)] : null;
};

/// Which rows are picked, settled the way the clip list settles it: plain for
/// this row alone, Ctrl for one more or one fewer, Shift for everything
/// between here and where the last plain press was.
/// Mark the picked rows on the rows that are already on screen.
///
/// Not a repaint of the list. A press that rebuilt the markup would be a press
/// that threw away the row it was on, and the second half of a double click
/// would land on a row that had not been there for the first half -- no
/// double click at all, as far as the window is concerned.
function paintPicked() {
  const rows = el("batch-list").children;
  for (let i = 0; i < rows.length; i += 1) {
    const job = batchJobs[i];
    rows[i].classList.toggle("picked", !!job && batchPicked.has(job.path));
  }
  // What can be done to the queue is mostly what can be done to the picking.
  paintBatchButtons();
}

function pickJob(job, ev) {
  const at = batchJobs.indexOf(job);
  if (ev.shiftKey && batchAnchor >= 0) {
    const [lo, hi] = [Math.min(batchAnchor, at), Math.max(batchAnchor, at)];
    batchPicked = new Set(batchJobs.slice(lo, hi + 1).map((j) => j.path));
  } else if (ev.ctrlKey || ev.metaKey) {
    if (batchPicked.has(job.path)) batchPicked.delete(job.path);
    else batchPicked.add(job.path);
    batchAnchor = at;
  } else {
    batchPicked = new Set([job.path]);
    batchAnchor = at;
  }
  paintPicked();
}

/// Picking rows and starting to carry them are the same press: which of the
/// two it was is settled by whether the pointer travels. A press on a row
/// that is already picked leaves the picking alone until the button comes
/// up -- collapsing to the one row on the way down would drop the other four
/// out of the drag before it began, which is the clip list's reason too.
el("batch-list").addEventListener("mousedown", (ev) => {
  if (ev.button !== 0) return;
  const job = jobAt(ev);
  // A press on the list where there is no row is a press on nothing, and
  // that is an answer rather than an accident -- the same answer the clip
  // list gives it, for the same reason: it is how twenty picked rows are let
  // go of without having to find one of them to click on. The right button is
  // left out here as it is there, since it opens a menu and a menu about
  // nothing is not worth unpicking rows for.
  if (!job) {
    if (!batchPicked.size) return;
    batchPicked.clear();
    batchAnchor = -1;
    paintPicked();
    return;
  }
  if (ev.target.closest("[data-stop]") || ev.target.closest("[data-kill]")) return;
  const plain = !ev.shiftKey && !ev.ctrlKey && !ev.metaKey;
  jobPress = {
    path: job.path,
    x: ev.clientX,
    y: ev.clientY,
    collapse: batchPicked.has(job.path) && plain,
  };
  if (!jobPress.collapse) pickJob(job, ev);
});

/// What a double click does to a clip is open it; what it does to a job is
/// open the project the job is, which is the same act one window further out.
/// Enter does it too, as it does there.
el("batch-list").addEventListener("dblclick", (ev) => {
  const job = jobAt(ev);
  if (!job) return;
  if (ev.target.closest("[data-stop]") || ev.target.closest("[data-kill]")) return;
  openJobProject();
});

el("batch-list").addEventListener("contextmenu", (ev) => {
  const job = jobAt(ev);
  if (!job) return;
  ev.preventDefault();
  // The menu is about what is picked, and a row nobody had picked becomes the
  // picking by being right-clicked. A row already in one is left alone:
  // collapsing five rows to the one under the pointer would leave the menu
  // doing its work to rows nobody asked it about.
  if (!batchPicked.has(job.path)) pickJob(job, {});
  openJobMenu(ev.clientX, ev.clientY);
});

/// ジョブ追加: the queue takes a copy of what was picked and runs the copy.
///
/// Same answer as バッチに登録's, and for the same reasons: a row pointing at
/// somebody's own file is a row that changes when they edit it and breaks
/// when they move it, and the file could not then be thrown away with the
/// row. What they picked is left exactly as it was.
///
/// The row is named after the file it was made from, which is also what a
/// second helping of the same project is recognised by -- the copy has a name
/// of the queue's choosing and no two copies ever share one, so the path
/// cannot answer that question any more.
async function addPickedProject(path) {
  const label = stemOf(path);
  if (batchJobs.some((j) => j.label === label)) {
    sayHere(t("batch.already", { name: label }));
    return;
  }
  let copy;
  try {
    copy = await invoke("queue_copy", { path });
  } catch (e) {
    sayHere(String(e));
    return;
  }
  // A copy of a file that turns out not to be a project is a copy of nothing:
  // `addBatchJob` has already said so, and the queue should not be left
  // holding it.
  if (!(await addBatchJob(copy, label))) await dropQueuedCopies([{ path: copy }]);
}

el("batch-add").addEventListener("click", async () => {
  const picked = await dialog.open({
    multiple: true,
    filters: [{ name: t("dialog.project"), extensions: [PROJECT_EXT] }],
  });
  if (!picked) return;
  for (const path of Array.isArray(picked) ? picked : [picked]) await addPickedProject(path);
});

/// The file the queue is to be given, written if it is not already there.
///
/// A job is a file: an unsaved list put in the queue would be a job that ran
/// whatever the file said at midnight rather than what is on screen now. So
/// the list becomes one -- written over its own file where it has one, and
/// otherwise written under a name worked out here.
///
/// Without a picker. The one question a save dialog asks is where, and at
/// this press there is only one answer worth having: with the output, or with
/// the recordings -- which is where the picker would have opened anyway. A
/// dialog whose answer is already known is a dialog that stands between the
/// button and the thing it is named after. Somebody who wants the project
/// somewhere else has 名前を付けて保存 for that, and it can be moved after.
///
/// Empty for a file that could not be written, which `writeProject` has
/// already said its own sentence about.
async function projectForQueue() {
  // Always a copy, in the queue's own folder. Never the project this window
  // has open, and never a file in the output folder.
  //
  // The queue owns what it runs. A row pointing at somebody's own project is
  // a row whose job changes when they edit that project and breaks when they
  // move it, and it could not be thrown away with the row; a `.scproj` nobody
  // asked for, left in the output folder after the job has run, is litter
  // with nothing to say when it should go. In the queue's folder the file is
  // the queue's, and goes when the job goes -- see `drop_temp_project`.
  //
  // Which is also why the copy does not become the project this window is
  // about. The title bar would then name a file in a folder nobody can find,
  // and 保存 would write there instead of over the project that is open, or
  // instead of asking for a name.
  //
  // **Registering counts as saving all the same.** The list goes on disc
  // either way, so a window still saying there is work to lose would be
  // wrong -- and it would be wrong about work somebody has just put where
  // they meant it to go. So the project that is open is written first where
  // there is one, the way 保存 writes it; and where there is none, the copy
  // is what there is to have been saved, and the `*` comes off for it.
  if (projectPath && dirty() && !(await writeProject(projectPath))) return "";
  const stem = projectPath ? stemOf(projectPath) : autoProjectStem();
  let path = tempProject;
  if (!path) {
    try {
      path = await invoke("queue_temp_path", { stem, ext: PROJECT_EXT });
    } catch (e) {
      sayHere(String(e));
      return "";
    }
  }
  if (!(await putProject(path, true, true))) return "";
  // Kept, so that a list registered twice is written to the one file and
  // refused as the duplicate it is, rather than piling up copies of itself.
  tempProject = path;
  // An untitled list has no file of its own to have been written, and the
  // copy is what it now has instead. Where there was a project, `writeProject`
  // has already said as much.
  if (!projectPath) {
    savedShape = shapeOf();
    retitleMain();
  }
  return path;
}

/// バッチに登録: put the list on screen into the queue, and open the tool
/// over it.
///
/// The tool is started because the press means the work is to be written this
/// way rather than now, and a queue with nothing over it is written by
/// nobody. One already running is the same request granted -- see
/// `open_batch_tool` -- and the tool picks the row up within a couple of
/// seconds of it landing; see `watchQueue`.
///
/// The queue is read back first because this window does not otherwise hold
/// it: without that, a list already in the queue would be reported as added
/// even though the backend refused it as a duplicate.
el("enlist-export").addEventListener("click", async () => {
  if (!clips.length) {
    sayHere(t("project.nothingToSave"));
    return;
  }
  // The same button, in a window that was opened out of the queue: what it
  // has open is a job, and the thing to do with an edited job is put it back.
  if (queuedJob) {
    await overwriteQueuedJob();
    return;
  }
  const label = projectPath ? stemOf(projectPath) : autoProjectStem();
  const path = await projectForQueue();
  if (!path) return;
  await refreshQueue();
  if (!(await addBatchJob(path, label))) return;
  sayHere(t("batch.added", { name: label }));
  try {
    await invoke("open_batch_tool");
  } catch (e) {
    sayHere(String(e));
  }
});

/// バッチを上書き: write this list back into the job the window was opened on.
///
/// No queue to add to and no copy to make: the file is already a row of the
/// queue, and this is the one window that is allowed to write it. The tool is
/// told, because what it shows about a project -- how many recordings, the
/// picture off the first of them -- it read once and kept.
///
/// The row keeps its place and its state. A job already written that is
/// edited here does not go back to 待機 on its own: whether it is to be
/// written again is もう一度出力する, on the row, where it was before.
async function overwriteQueuedJob() {
  if (!(await putProject(queuedJob, true))) return;
  await touchQueuedJob(queuedJob);
  // This list is now what is on disc, wherever the title bar is pointed.
  if (projectPath === queuedJob) {
    savedShape = shapeOf();
    retitleMain();
  }
  sayHere(t("batch.overwritten", { name: stemOf(queuedJob) }));
}

/// Say that a queued project has been written over, where it was this
/// window's own job. Quiet about failing: the file is written either way, and
/// the worst of a queue that did not hear is a row saying how many recordings
/// the job held an hour ago.
async function touchQueuedJob(path) {
  if (!invoke || path !== queuedJob) return;
  try {
    // With the sentence the row was given when the job was added, said again
    // about the list as it now stands: it counts the recordings, and this is
    // the window that has just changed how many there are.
    await invoke("batch_touch", { path, note: t("batch.clips", { n: clips.length }) });
  } catch {
    /* the file is written, which is what the press was about */
  }
}

/// Move the picked rows, which is what the right button's menu offers and
/// what a short drag comes to.
///
/// One place at a time is the clip list's step, and for the same reason it is
/// written the same way: each picked row moves into the gap beside it unless
/// the row it would swap with is picked as well, so five rows carried up
/// together arrive still five rows together. A step bigger than the queue is
/// 先頭へ移動 and 末尾へ移動 -- the picked rows out and put back at the end
/// they were sent to, in the order they were in.
async function moveBatch(by) {
  if (batchRunning || !batchPicked.size) return;
  if (Math.abs(by) === 1) {
    const order = by < 0
      ? batchJobs.map((_, i) => i)
      : batchJobs.map((_, i) => batchJobs.length - 1 - i);
    let moved = false;
    for (const i of order) {
      const j = i + by;
      if (!batchPicked.has(batchJobs[i].path)) continue;
      if (j < 0 || j >= batchJobs.length || batchPicked.has(batchJobs[j].path)) continue;
      [batchJobs[i], batchJobs[j]] = [batchJobs[j], batchJobs[i]];
      moved = true;
    }
    if (!moved) return;
  } else {
    const held = pickedJobs();
    const rest = batchJobs.filter((j) => !batchPicked.has(j.path));
    batchJobs = by < 0 ? [...held, ...rest] : [...rest, ...held];
  }
  await saveQueue();
  renderBatch();
}

// --- the menu on the right button, and carrying a row -------------------
//
// Where a job goes in the queue is the one thing that is about *that* job
// rather than about the queue: adding and removing are on the bar, where they
// are about the queue as a whole. So it is on the row -- under the right
// button, and under the pointer that drags it.
//
// Both follow the clip list, which does the same two things to the same kind
// of row; see 並べ替え（ドラッグ）there. Plain mouse events rather than HTML5
// drag and drop, because Tauri takes the window's drags before the page sees
// them. What is carried is the picking, so the press that would pick one row
// may also be the start of carrying five.

const jobMenu = el("job-menu");

/// A declaration rather than a `const`, because `show` is above this and puts
/// the menu away.
function closeJobMenu() {
  if (jobMenu) jobMenu.hidden = true;
}

function openJobMenu(x, y) {
  const top = firstPicked();
  const bottom = lastPicked();
  // The two that are about one file rather than about a stretch of the queue.
  const one = onePicked();
  const look = (one && jobLook.get(one.path)) || {};
  // Moving is the loop's business while it runs: it walks the rows by place.
  el("job-top").disabled = batchRunning || top <= 0;
  el("job-up").disabled = batchRunning || top <= 0;
  el("job-down").disabled = batchRunning || bottom < 0 || bottom >= batchJobs.length - 1;
  el("job-bottom").disabled = batchRunning || bottom < 0 || bottom >= batchJobs.length - 1;
  // Only a job that has finished with this run has anything to put back, and
  // the one being written is not finished with it. Live for a picking with
  // any such row in it, and it is those rows it puts back.
  el("job-requeue").disabled = !pickedJobs().some(canRequeue);
  el("job-open").disabled = !one;
  // A project that writes beside its recordings has no folder of its own to
  // be opened at.
  el("job-folder").disabled = !one || !look.dir;
  el("job-remove").disabled = batchRunning || !batchPicked.size;
  showBatchMenu(false);
  jobMenu.style.left = `${x}px`;
  jobMenu.style.top = `${y}px`;
  jobMenu.hidden = false;
  // Measured once it is up and its labels are in it, and held inside the
  // window, the way the clip list's own menu is.
  const box = jobMenu.getBoundingClientRect();
  jobMenu.style.left = `${Math.max(0, Math.min(x, window.innerWidth - box.width - 2))}px`;
  jobMenu.style.top = `${Math.max(0, Math.min(y, window.innerHeight - box.height - 2))}px`;
}

el("job-up").addEventListener("click", () => {
  closeJobMenu();
  moveBatch(-1);
});
el("job-down").addEventListener("click", () => {
  closeJobMenu();
  moveBatch(1);
});
el("job-top").addEventListener("click", () => {
  closeJobMenu();
  moveBatch(-batchJobs.length);
});
el("job-bottom").addEventListener("click", () => {
  closeJobMenu();
  moveBatch(batchJobs.length);
});

/// もう一度出力する: put a job that is finished with this run back in the
/// queue as one that is not.
///
/// The one thing a queue could not do at all. A job that has been written
/// stays written -- which is what keeps a second run from writing it twice --
/// and the only way back was to take it out and put it in again, losing its
/// place. What it was told about the last run goes with it: a row that says
/// it is waiting should not also be saying how long it took.
el("job-requeue").addEventListener("click", async () => {
  closeJobMenu();
  const back = pickedJobs().filter(canRequeue);
  if (!back.length) return;
  for (const job of back) {
    job.state = "waiting";
    job.note = t("batch.waiting");
    job.began = undefined;
    job.spent = undefined;
    job.done = 0;
    job.since = undefined;
    // Including the frame the last run left on it, which was a picture of
    // work this row is no longer saying it has done.
    job.shot = undefined;
    job.now = undefined;
  }
  await saveQueue();
  renderBatch();
});

/// プロジェクトを開く: hand the job's file to a list window of its own.
async function openJobProject() {
  const job = onePicked();
  if (!job) return;
  try {
    await invoke("open_project_window", { path: job.path, queued: true });
  } catch (e) {
    sayHere(String(e));
  }
}
el("job-open").addEventListener("click", () => {
  closeJobMenu();
  openJobProject();
});

/// 出力先フォルダーを開く: the one question a finished queue leaves.
el("job-folder").addEventListener("click", async () => {
  closeJobMenu();
  const job = onePicked();
  const look = (job && jobLook.get(job.path)) || {};
  if (!look.dir) return;
  try {
    await invoke("show_folder", { path: look.dir });
  } catch (e) {
    sayHere(String(e));
  }
});

/// ジョブ削除, from under the pointer rather than from the bar.
el("job-remove").addEventListener("click", () => {
  closeJobMenu();
  dropPicked();
});

// A press anywhere else shuts it -- `mousedown`, because the press that
// opened it was a right button and a right button elsewhere is a click that
// never arrives.
window.addEventListener("mousedown", (ev) => {
  if (!ev.target.closest("#job-menu")) closeJobMenu();
});
window.addEventListener("wheel", closeJobMenu, true);
window.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") closeJobMenu();
});

/// Where the pointer would put the row: an index into `batchJobs` counted the
/// way an insertion is -- 0 above the first, `length` below the last. The
/// half-way line of a row is where it changes.
function jobDropAt(y) {
  const rows = el("batch-list").children;
  for (let i = 0; i < rows.length; i++) {
    const r = rows[i].getBoundingClientRect();
    if (y < r.top + r.height / 2) return i;
  }
  return rows.length;
}

/// The carried rows dimmed, and a line where they would land.
function paintJobDrag() {
  const rows = el("batch-list").children;
  for (let i = 0; i < rows.length; i++) {
    const job = batchJobs[i];
    rows[i].classList.toggle("dragging", !!jobDrag && !!job && jobDrag.paths.has(job.path));
    rows[i].classList.toggle("dropbefore", !!jobDrag && jobDrag.at === i);
    rows[i].classList.toggle(
      "dropafter",
      !!jobDrag && jobDrag.at === rows.length && i === rows.length - 1
    );
  }
}

function clearJobDrag() {
  jobPress = null;
  if (!jobDrag) return;
  jobDrag = null;
  el("batch-list").classList.remove("reordering");
  paintJobDrag();
}

/// Take the carried rows out and put them back in at the drop, keeping the
/// order they were in. The index counted rows that are being carried, so what
/// it means once they are out is however many of the rows left were above it.
async function endJobDrag() {
  const { paths, at } = jobDrag;
  const held = batchJobs.filter((j) => paths.has(j.path));
  const rest = batchJobs.filter((j) => !paths.has(j.path));
  const above = batchJobs.slice(0, at).filter((j) => !paths.has(j.path)).length;
  clearJobDrag();
  if (!held.length) return;
  const was = batchJobs;
  batchJobs = [...rest.slice(0, above), ...held, ...rest.slice(above)];
  // Put down where they were picked up: nothing to write, and nothing to
  // repaint that the drag's own classes have not already taken off.
  if (batchJobs.every((j, i) => j === was[i])) {
    paintJobDrag();
    return;
  }
  await saveQueue();
  renderBatch();
}

window.addEventListener("mousemove", (ev) => {
  if (!jobPress && !jobDrag) return;
  if (!jobDrag) {
    // Far enough that a click under an unsteady hand is still a click.
    if (Math.abs(ev.clientX - jobPress.x) + Math.abs(ev.clientY - jobPress.y) < 4) return;
    if (batchRunning) return;
    // What is carried is the picking, unless the press was on a row outside
    // it -- which cannot happen from here, the press having picked that row
    // on the way down, but is what the row under the pointer means.
    const held = batchPicked.has(jobPress.path) ? pickedJobs() : batchJobs.filter((j) => j.path === jobPress.path);
    jobDrag = {
      paths: new Set(held.map((j) => j.path)),
      at: batchJobs.findIndex((j) => j.path === jobPress.path),
    };
    el("batch-list").classList.add("reordering");
  }
  jobDrag.y = ev.clientY;
  jobDrag.at = jobDropAt(ev.clientY);
  paintJobDrag();
  jobEdge();
});

window.addEventListener("mouseup", () => {
  if (jobDrag) endJobDrag();
  // A press that never travelled: the picking it was holding open now settles
  // onto the one row, which is what a plain click has always meant.
  else if (jobPress && jobPress.collapse) {
    const job = batchJobs.find((j) => j.path === jobPress.path);
    if (job) pickJob(job, {});
  }
  jobPress = null;
});

// Escape puts it back, and so does the pointer leaving the window: neither
// should land a row somewhere unasked.
window.addEventListener(
  "keydown",
  (ev) => {
    if (!jobDrag || ev.key !== "Escape") return;
    ev.stopPropagation();
    clearJobDrag();
  },
  true
);
window.addEventListener("blur", () => clearJobDrag());

/// Reaching the ends of a long queue without letting go: while the pointer is
/// held near the top or bottom of the list, the list comes to it.
function jobEdge() {
  if (!jobDrag) return;
  const wrap = el("batch-list");
  const r = wrap.getBoundingClientRect();
  const EDGE = 28;
  const over = Math.min(jobDrag.y - (r.top + EDGE), 0) || Math.max(jobDrag.y - (r.bottom - EDGE), 0);
  if (!over) return;
  const was = wrap.scrollTop;
  wrap.scrollTop += Math.max(-EDGE, Math.min(EDGE, over)) * 0.5;
  if (wrap.scrollTop !== was) {
    jobDrag.at = jobDropAt(jobDrag.y);
    paintJobDrag();
  }
}

/// ジョブ削除: the picked rows, from the bar or from the row's own menu.
///
/// Nothing is picked afterwards, as nothing is selected in the clip list once
/// what was selected has been removed: the rows that answered to the pick are
/// gone, and putting the pick onto whichever row moved up into the gap would
/// be the program choosing one nobody pointed at.
async function dropPicked() {
  if (batchRunning || !batchPicked.size) return;
  const gone = pickedJobs();
  batchJobs = batchJobs.filter((j) => !batchPicked.has(j.path));
  batchPicked.clear();
  batchAnchor = -1;
  await saveQueue();
  renderBatch();
  await dropQueuedCopies(gone);
}

/// Jobs have left the queue for good: throw away the projects that were the
/// queue's own copies, and leave every other project alone.
///
/// Which is which is the backend's to say -- it is the folder the file is in,
/// see `drop_temp_project` -- so this hands over every path and reads nothing
/// back. Last, after the queue is written and the rows are gone: the removal
/// is what was asked for, and a file that will not go is no reason to keep a
/// row nobody wants.
async function dropQueuedCopies(gone) {
  for (const job of gone) {
    try {
      await invoke("drop_temp_project", { path: job.path });
    } catch {
      /* the row is out, which is what the press was about */
    }
  }
}
el("batch-drop").addEventListener("click", dropPicked);

el("batch-more").addEventListener("click", (ev) => {
  ev.stopPropagation();
  showBatchMenu(el("batch-menu").hidden);
});
// Anywhere else, and Escape, the way the menu in the other corner goes away.
window.addEventListener("click", () => showBatchMenu(false));
window.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") showBatchMenu(false);
});

/// 出力済みのジョブを削除: the rows that have nothing left to do.
///
/// No question asked, unlike すべて削除. What this takes out is the part of
/// the queue that has already happened, and the rows it leaves are exactly
/// the ones somebody would have been picking out one at a time.
el("batch-clear-done").addEventListener("click", async () => {
  showBatchMenu(false);
  const gone = batchJobs.filter((j) => j.state === "done");
  if (!gone.length) return;
  batchJobs = batchJobs.filter((j) => j.state !== "done");
  for (const path of [...batchPicked]) {
    if (!batchJobs.some((j) => j.path === path)) batchPicked.delete(path);
  }
  await saveQueue();
  renderBatch();
  sayHere(t("batch.clearedDone", { n: gone.length }));
  await dropQueuedCopies(gone);
});

/// すべて削除, which is asked about: the rows still waiting are work somebody
/// lined up, and there is no putting them back.
el("batch-clear-all").addEventListener("click", async () => {
  showBatchMenu(false);
  if (!batchJobs.length) return;
  const go = await dialog.ask(t("batch.clearBody", { n: batchJobs.length }), {
    title: t("batch.clearTitle"),
    kind: "warning",
  });
  if (!go) return;
  const gone = batchJobs;
  batchJobs = [];
  batchPicked.clear();
  batchAnchor = -1;
  await saveQueue();
  renderBatch();
  await dropQueuedCopies(gone);
});

/// Start the tool, which is a second process of this same program.
///
/// From the menu rather than from a screen, because that is what it is: a
/// window of its own, opened the way a program is opened. Never two over one
/// queue; asked for by name like this, a tool that was already up is worth
/// saying so about, which is the whole of the difference between this caller
/// and バッチに登録.
async function openBatchTool() {
  try {
    sayHere(t((await invoke("open_batch_tool")) ? "batch.opened" : "batch.alreadyUp"));
  } catch (e) {
    sayHere(String(e));
  }
}

/// The answer 完了後 currently holds, said on the row that opens it, and the
/// tick beside whichever of the three it is.
function paintAfterMenu() {
  const now = el("menu-after-now");
  if (now) now.textContent = t(`batch.after${batchAfter[0].toUpperCase()}${batchAfter.slice(1)}`);
  for (const which of ["nothing", "sleep", "shutdown"]) {
    const item = el(`menu-after-${which}`);
    if (item) item.classList.toggle("on", batchAfter === which);
  }
}

/// Fold the three away or out. A `function` because opening the menu closes
/// this, and that happens above here.
function showAfterFold(on) {
  const head = el("menu-after");
  if (!head) return;
  head.classList.toggle("open", !!on);
  head.setAttribute("aria-expanded", String(!!on));
  el("menu-after-body").hidden = !on;
}

el("menu-after").addEventListener("click", (ev) => {
  // An item that opens rather than does: the menu stays up.
  ev.stopPropagation();
  showAfterFold(el("menu-after-body").hidden);
});

for (const which of ["nothing", "sleep", "shutdown"]) {
  el(`menu-after-${which}`).addEventListener("click", async () => {
    showMenu(false);
    batchAfter = which;
    paintAfterMenu();
    await saveQueue();
    // A choice made while the countdown is already running is about the next
    // run, not this one: the queue it was going to act on is already empty.
    cancelAfter();
  });
}

el("batch-go").addEventListener("click", runBatch);

/// すべて中止: this job and every job behind it.
el("batch-stop-all").addEventListener("click", () => {
  if (!batchRunning) return;
  batchStopped = true;
  // And the job under the head. Stopping the queue and letting the disc it
  // is halfway through finish would be a stop nobody asked for.
  abort = true;
  // Said on the row it is about. The one being written takes a moment to
  // stop -- it finishes the recording in hand -- and a row that went on
  // saying what it was writing would look like one that had not been told.
  const now = batchJobs.find((j) => j.state === "running");
  if (now) now.note = t("batch.stopping");
  if (exporting) el("out-state").textContent = t("out.aborting");
  renderBatch();
});

/// A job called off on its own, from the row it is on.
///
/// The one being written stops the way 出力中止 stops it -- the recording in
/// hand is finished first, so nothing half-written is left behind -- and the
/// queue goes on to the next job. One whose turn has not come is simply
/// passed over when the loop reaches it. Either way the row says so, and the
/// job is waiting again at the next バッチ開始: calling a job off is about
/// this run, and deleting it is what the bar is for.
/// The × in a row's corner: take that job out of the queue. Only while
/// nothing is running -- the loop is walking these by place, and a row pulled
/// out from under it would shift the ones behind. Calling a job off is what
/// there is for a queue in flight, and that is the 中止 on the row.
el("batch-list").addEventListener("click", async (ev) => {
  const at = ev.target.closest("[data-kill]");
  if (!at || batchRunning) return;
  ev.stopPropagation();
  const job = batchJobs[Number(at.dataset.kill)];
  if (!job) return;
  batchJobs = batchJobs.filter((j) => j !== job);
  batchPicked.delete(job.path);
  await saveQueue();
  renderBatch();
  await dropQueuedCopies([job]);
});

el("batch-list").addEventListener("click", (ev) => {
  const at = ev.target.closest("[data-stop]");
  if (!at) return;
  ev.stopPropagation();
  const job = batchJobs[Number(at.dataset.stop)];
  if (!job || !canStop(job)) return;
  // The same mark either way, because the loop takes it the same way: at the
  // next moment control comes back to it. `abort` is the other half, and only
  // for a job whose cut is already running -- it is what stops that, and it
  // is read by the pass rather than by the loop.
  const running = job.state === "running";
  job.state = "skipped";
  // Stopping, until it has: the job being written finishes the recording in
  // hand first, and the loop writes the final word when it comes back to it.
  job.note = t(running ? "batch.stopping" : "batch.jobStopped");
  if (running) {
    abort = true;
    if (exporting) el("out-state").textContent = t("out.aborting");
  }
  renderBatch();
});

/// Wait until the index lane has finished with every row of the list it has
/// just been handed.
///
/// `ready()` is what the output pass writes, and a row is only in it once it
/// has been read. Pressing 出力開始 the instant a project opens would write
/// whichever rows happened to be done -- which by hand is impossible to do,
/// because by hand there is a person waiting for the list to settle.
function listSettled() {
  return new Promise((done) => {
    const tick = () => {
      if (batchStopped) return done();
      if (!clips.some((c) => c.state === "queued" || c.state === "indexing")) return done();
      setTimeout(tick, 250);
    };
    tick();
  });
}

/// Walk the queue.
///
/// Every job is attempted. One that fails is recorded as failed and the next
/// one starts: a queue left overnight is left because nobody is there to
/// answer a question, and a run that stopped at job two because job two's
/// folder was full would have wasted the night on the four behind it. Only
/// すべて中止 stops the walk; a row's own 中止 stops only that job.
async function runBatch() {
  if (batchRunning || exporting) return;
  await refreshQueue();
  if (!batchJobs.some((j) => j.state !== "done")) return;
  // The list on screen is about to be replaced, once per job. Asked once,
  // here, and never again while the queue runs.
  if (!(await askReplace(t("batch.replaceTitle"), t("batch.replaceBody")))) return;
  cancelAfter();
  batchRunning = true;
  batchStopped = false;
  // The clock on the running row moves between progress reports, and there
  // are none at all while a project is opening. The frame and the sentence
  // are taken at the same beat: the stage moves on at the head of each
  // recording, which is a moment no report falls on.
  const ticking = setInterval(() => {
    if (!batchRunning) return;
    const job = runningJob();
    if (job) followJob(job);
    renderBatch();
  }, 1000);
  for (const j of batchJobs) {
    if (j.state === "done") continue;
    j.state = "waiting";
    j.note = t("batch.waiting");
  }
  renderBatch();
  await saveQueue();
  try {
    await walkQueue();
  } catch (e) {
    // The walk itself, rather than a job in it: each job already answers for
    // its own failures. Caught all the same, so that the three lines below
    // run and the queue is written down as it actually stands -- a file left
    // saying a job was running would be a row nothing is doing anything about.
    sayHere(t("batch.threw", { e: String(e) }));
    for (const job of batchJobs) {
      if (job.state !== "running") continue;
      job.state = "error";
      job.note = t("batch.threw", { e: String(e) });
    }
  } finally {
    // Whatever happened in there, this window is not writing any more. The
    // two are together because a queue that said it was running with nothing
    // running is a queue whose バッチ開始 is switched off for good.
    batchRunning = false;
    clearInterval(ticking);
  }
  show("batch");
  renderBatch();
  await saveQueue();
  // The machine is only put down over a queue that ran to the end. A queue
  // somebody stopped is a queue somebody is standing at.
  if (!batchStopped) startAfter();
}

/// The walk itself, one job after another. Split from the press above so
/// that the two lines that say the queue has stopped running are written
/// once, in a `finally`, rather than at every way out of it.
async function walkQueue() {
  // By index rather than over the array, because the array is replaced
  // whenever a job is added from the other window; see `takeAdded`.
  for (let i = 0; i < batchJobs.length; i += 1) {
    const job = batchJobs[i];
    if (job.state === "done") continue;
    // Called off from its own row before its turn came. The note it carries
    // is the one that click left on it.
    if (job.state === "skipped") continue;
    if (batchStopped) {
      job.state = "skipped";
      job.note = t("batch.skipped");
      continue;
    }
    job.state = "running";
    job.note = t("batch.opening");
    job.began = Date.now();
    job.done = 0;
    renderBatch();
    await saveQueue();
    // Called off from its own row while this was going on. The click sets the
    // state and the note; all that is left here is to take the hint at the
    // next moment control comes back, which is why it is asked after each of
    // the three waits below rather than once.
    const calledOff = () => job.state === "skipped";
    // **A job that throws is a job that failed, and nothing more than that.**
    // Every await below is over code written for somebody standing at the
    // window, where a mistake is a message and the next press puts it right.
    // There is nobody here, and an error let out of this loop would stop the
    // walk where it stood -- leaving the queue saying it was running, with
    // its one way to start another run switched off, until the tool was
    // killed. Which is the one thing this screen is for: the queue goes on.
    try {
      if (!(await loadProject(job.path))) {
        job.state = "error";
        job.note = t("batch.cannotOpen");
      } else if (calledOff()) {
        // Nothing to do: the note is the one the click left.
      } else {
        job.note = t("batch.reading");
        renderBatch();
        await listSettled();
        const list = ready();
        if (calledOff()) {
          // As above.
        } else if (batchStopped) {
          job.state = "skipped";
          job.note = t("batch.skipped");
        } else if (!list.length) {
          job.state = "error";
          job.note = t("batch.nothingReadable");
        } else {
          job.note = t("batch.writing");
          renderBatch();
          await runExport();
          // `runExport` turns back at the door onto the settings screen when
          // a disc has nowhere to be written. There is nobody on that screen
          // here, and this window is its queue.
          show("batch");
          if (calledOff() || abort) {
            // Either this job was called off or the whole queue was; which of
            // them decides whether there is a next job.
            job.state = "skipped";
            job.note = t(batchStopped ? "batch.stoppedHere" : "batch.jobStopped");
          } else if (list.every((c) => c.out.state === "idle")) {
            // A run that never started: the guard above sent it back without
            // writing anything.
            job.state = "error";
            job.note = t("batch.refused");
          } else {
            const bad = list.filter((c) => c.out.state === "error").length;
            const good = list.filter((c) => c.out.state === "done").length;
            job.state = bad ? "error" : "done";
            job.note = bad
              ? t("batch.someFailed", { n: bad, all: list.length })
              : t("batch.wrote", { n: good });
          }
        }
      }
    } catch (e) {
      job.state = "error";
      job.note = t("batch.threw", { e: String(e) });
      // And the pass it was thrown out of, if it was in one: what `runExport`
      // sets on its way in it clears on its way out, and an error between the
      // two leaves this window saying it is still writing -- which is a
      // window that will not start the next job. Put down here rather than
      // left to the next caller, the way the end of the pass puts them down.
      // `abort` is not among them: `runExport` clears it as it starts.
      exporting = false;
      paused = false;
      runDir = null;
      runFolder = null;
      writing = null;
      paintExportButton();
      pump();
      show("batch");
    }
    // Where its clock stops. Read back by the row from here on, so that a
    // finished job goes on saying how long it took rather than how long ago
    // it started.
    if (job.began) job.spent = (Date.now() - job.began) / 1000;
    renderBatch();
    // Written down as each job ends, and the jobs added while it ran are
    // taken at the same moment. Both halves of that are the file: the list
    // window appends to it, and this is where what it appended is noticed.
    await takeAdded();
  }
}

/// Put the queue down, and pick up any job the other window added while this
/// one was busy.
///
/// The rows in hand win for everything they are about -- they are what has
/// just been written -- and anything in the file this loop has never seen is
/// added to the end, which is where the other window put it.
async function takeAdded() {
  let queue = null;
  try {
    queue = await invoke("batch_read");
  } catch {
    queue = null;
  }
  const added = ((queue && queue.jobs) || [])
    .filter((j) => j && j.path && !batchJobs.some((mine) => mine.path === j.path))
    .map((j) => ({
      path: j.path,
      label: j.label || stemOf(j.path),
      state: "waiting",
      note: j.note || t("batch.waiting"),
    }));
  batchJobs = batchJobs.concat(added);
  if (added.length) renderBatch();
  await saveQueue();
}

/// The countdown to sleeping or shutting down.
///
/// A minute, and a button. Failures do not cancel it: 完了後 is an answer
/// about the queue finishing rather than about it succeeding, and a run that
/// wrote nine discs and failed the tenth is still a run somebody went to bed
/// over. What the failures get is the list, which is on screen when the
/// machine comes back.
function startAfter() {
  if (batchAfter === "nothing" || !invoke) return;
  const what = batchAfter;
  let left = 60;
  el("batch-countdown-row").hidden = false;
  const tick = () => {
    el("batch-countdown").textContent = t(
      what === "sleep" ? "batch.sleepIn" : "batch.shutdownIn",
      { s: left }
    );
    if (left <= 0) {
      cancelAfter();
      invoke("after_batch", { what }).catch((e) => note(t("batch.afterFailed", { e: String(e) })));
      return;
    }
    left -= 1;
    afterTimer = setTimeout(tick, 1000);
  };
  tick();
}

function cancelAfter() {
  if (afterTimer) clearTimeout(afterTimer);
  afterTimer = null;
  el("batch-countdown-row").hidden = true;
}

el("batch-after-cancel").addEventListener("click", () => {
  cancelAfter();
  note(t("batch.afterCancelled"));
});

/// Which window this is, and what that window shows.
///
/// The tool is the same page as the list window because it needs the list and
/// the output screen to do the work -- it opens a project per job, the way a
/// person would. What it does not need is the two screens where a list is
/// built and settled: the answers on them belong to the project being run,
/// and a tool offering to change them would be offering to change something
/// it is about to read off a file. So they come off the bar.
///
/// And the other way round: the queue is the tool's, so its tab is off the
/// bar in the list window, which never shows it. What the list window does
/// with the queue it does from the output screen and the menu.
async function settleRole() {
  if (!invoke) return;
  try {
    batchRole = (await invoke("window_role")) || "main";
  } catch {
    batchRole = "main";
  }
  // And whether the project on the command line is a job out of the queue,
  // which is what プロジェクトを開く opens a window for. Before the project
  // itself is opened, so the button is right the first time it is painted.
  try {
    queuedJob = (await invoke("queued_job")) || "";
  } catch {
    queuedJob = "";
  }
  if (!isTool()) {
    await refreshQueue();
    return;
  }
  // No tabs at all. The tool has two screens and never a choice between
  // them: the queue while it is idle, and what is being written while it is
  // not, which `runBatch` moves between on its own.
  for (const tab of document.querySelectorAll(".screens .tab")) tab.hidden = true;
  el("batch-bar").hidden = false;
  // The three that are the tool's own answer about the machine, and the rule
  // and the label that group them.
  el("menu-after").hidden = false;
  paintAfterMenu();
  // And no way to start an export by hand. The list it is holding is a job
  // out of the queue; writing it again from underneath the queue is not
  // something the one control up on the bar should have a rival for.
  el("run-export").hidden = true;
  // Nor has it a list to put in the queue: what it has open is a job out of
  // the queue already. Nor another tool to open, being one.
  el("enlist-export").hidden = true;
  el("menu-batch").hidden = true;
  // Nor is there a project to save: what the tool opens it opens to write
  // out, and 保存 over the file it was handed is not something a queue should
  // be able to do on its own. The rules that group them go with them, or the
  // menu opens on two lines and two items.
  // The rule above them goes too, or the menu opens on a line. The one below
  // stays: it is what now separates 完了後 from the program's own items.
  for (const id of ["menu-new", "menu-open", "menu-save", "menu-save-as", "menu-sep-work"]) {
    if (el(id)) el(id).hidden = true;
  }
  show("batch");
  // The window was painted before this answer arrived, so whatever it put in
  // the title bar was the answer for the other kind of window.
  shownTitle = "";
  retitleMain();
  invoke("center_window");
  // Saying it is here, so a second tool is refused while this one runs. See
  // `batch_live`.
  invoke("batch_beat");
  setInterval(() => invoke("batch_beat"), 10000);
  await watchQueue();
  // Before the timer, so that a row left over from a tool that went away mid
  // job is mended once rather than found again on every poll.
  await takeBackInterrupted();
  setInterval(watchQueue, 2000);
}

// --- the program's own menu ----------------------------------------------
//
// One button in the corner and one item under it. It is not a menu bar and
// should not grow into one: everything about the *clips* is on the screens,
// and this is for the few things that are about the program.

const brand = el("brand");
const brandMenu = el("brand-menu");

function showMenu(on) {
  // Folded back each time, so that the menu opens on the same six lines it
  // opened on last time rather than on however it was left.
  showAfterFold(false);
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
//
// Every control here takes effect as it is changed: there is no OK, and the
// button at the foot only puts the panel away. A preferences screen with an
// Apply is a screen you can leave in a state that is neither what you had
// nor what you asked for, and this one is reachable while a list is being
// worked on.
//
// Three of them reach further than this window. The language and the two the
// cut editor reads are sent on as an event, because that window has its own
// copy of everything and does not read the store again while it is up; the
// four the engine acts on are sent to the backend, which is where a plan is
// made and where the scratch files are written. See `prefs.js` and
// `prefs.rs`.

const prefsPanel = el("prefs");

/// Which group is on screen. Kept for the life of the window rather than
/// stored: somebody who came back to the panel twice in a minute came back
/// for the same group, and somebody who starts the program again is starting
/// again.
let prefsPane = "view";

/// Put one group up and the rest away.
///
/// The list on the left is a tab list, so exactly one of its names is the
/// one that can be tabbed to -- the arrow keys are what walks the rest. See
/// the handler below.
function showPane(which) {
  prefsPane = which;
  for (const tab of document.querySelectorAll(".pref-tab")) {
    const on = tab.dataset.pane === which;
    tab.classList.toggle("active", on);
    tab.setAttribute("aria-selected", on ? "true" : "false");
    tab.tabIndex = on ? 0 : -1;
    el(`pref-pane-${tab.dataset.pane}`).hidden = !on;
  }
}

for (const tab of document.querySelectorAll(".pref-tab")) {
  tab.addEventListener("click", () => showPane(tab.dataset.pane));
}

/// The arrow keys walk the list, Home and End go to its ends. What a list of
/// tabs is expected to answer to, and the reason only the selected one is in
/// the tab order.
el("prefs").querySelector(".pref-tabs").addEventListener("keydown", (ev) => {
  const tabs = [...document.querySelectorAll(".pref-tab")];
  const at = tabs.findIndex((t) => t.dataset.pane === prefsPane);
  const to = {
    ArrowDown: at + 1,
    ArrowRight: at + 1,
    ArrowUp: at - 1,
    ArrowLeft: at - 1,
    Home: 0,
    End: tabs.length - 1,
  }[ev.key];
  if (to === undefined) return;
  ev.preventDefault();
  const next = tabs[clamp(to, 0, tabs.length - 1)];
  showPane(next.dataset.pane);
  next.focus();
});

/// Put the store on screen. Called on opening rather than once at startup:
/// nothing else writes these, but a panel that paints itself is a panel that
/// cannot be caught showing yesterday's answer.
function paintPrefs() {
  el("pref-lang").value = preference();
  el("pref-counter").checked = !!prefs.get("counter");
  el("pref-meter").checked = prefs.get("meter") !== false;
  el("pref-subs").checked = !!prefs.get("subsOn");
  for (const [id, name] of PAGE_STEPS.concat(RUN_LENGTHS)) {
    el(id).value = String(prefs.get(name));
    el(`${id}-unit`).value = String(prefs.get(`${name}Unit`));
    paintStepUnit(id, prefs.get(`${name}Unit`));
  }
  el("pref-quiet-level").value = String(prefs.get("quietLevel"));
  el("pref-sidecar").value = String(prefs.get("sidecarPriority"));
  el("pref-cm-keyframes").checked = prefs.get("cmKeyframes") !== false;
  el("pref-cm-inserts").checked = prefs.get("cmInserts") === true;
  el("pref-blank-shades").value = String(prefs.get("blankShades") || "both");
  for (const [id, name] of BLANK_LEVELS) el(id).value = String(prefs.get(name));
  el("pref-flat-mark-at").value = String(prefs.get("flatMarkAt") || "after");
  el("pref-blank-keyframes").checked = prefs.get("blankKeyframes") !== false;
  el("pref-quiet-keyframes").checked = prefs.get("quietKeyframes") !== false;
  el("pref-quiet-overwrite").checked = !!prefs.get("quietOverwrite");
  el("pref-prefix").value = String(prefs.get("outPrefix") ?? "");
  el("pref-number").checked = !!prefs.get("outNumber");
  el("pref-digits").value = String(Number(prefs.get("outDigits")) || 2);
  el("pref-audio-channels").value = String(prefs.get("outAudioChannels") ?? "");
  el("pref-data-broadcast").checked = prefs.get("dataBroadcast") !== false;
  el("pref-keep-output").checked = !!prefs.get("keepOutput");
  el("pref-clean-joins").checked = !!prefs.get("cleanJoins");
  el("pref-proxy").checked = !!prefs.get("proxy");
  el("pref-proxy-width").value = String(Number(prefs.get("proxyWidth")) || 0);
  el("pref-ffmpeg-log").value = String(Number(prefs.get("ffmpegLog")) || 0);
  el("pref-audio-fade").value = String(Number(prefs.get("audioFade")) || 0);
  paintCacheDir();
  paintKeptOutput();
  paintProxyWidth();
  paintPrefDigits();
}

/// The digits, only while a number is being put on at all. Hidden rather than
/// greyed, which is what the proxy's width does one group along: these are
/// rows of their own here, and a row that is not an answer to anything is a
/// row to read past.
function paintPrefDigits() {
  el("row-pref-digits").hidden = !prefs.get("outNumber");
}

/// The width only means anything while a proxy is being built at all.
function paintProxyWidth() {
  el("row-pref-proxy-width").hidden = !prefs.get("proxy");
}

function paintCacheDir() {
  const dir = String(prefs.get("cacheDir") || "");
  const field = el("pref-cache-dir");
  field.value = dir;
  field.placeholder = cacheHome || t("prefs.cacheDirDefault");
}

/// What a restart would put back, for the line under the preference. The
/// folder alone: it is the setting somebody would be surprised to inherit,
/// and the rest of them are on the screen the panel is standing over.
function paintKeptOutput() {
  const kept = prefs.get("output");
  const line = el("pref-keep-what");
  if (!kept || typeof kept !== "object") {
    line.textContent = t("prefs.keepNone");
    return;
  }
  line.textContent = t("prefs.keepWhat", { what: kept.dir || t("prefs.keepBeside") });
}

function showPrefs(on) {
  prefsPanel.hidden = !on;
  if (!on) return;
  showPane(prefsPane);
  paintPrefs();
  // What is actually on disk, which only the other side can say. Asked on
  // every opening because a pass that ran while the panel was shut has added
  // to it.
  paintCacheUse();
}

el("menu-batch").addEventListener("click", () => {
  showMenu(false);
  openBatchTool();
});
el("menu-prefs").addEventListener("click", () => {
  showMenu(false);
  showPrefs(true);
});
el("prefs-close").addEventListener("click", () => showPrefs(false));
// The dark ground behind the panel, but not the panel itself.
prefsPanel.addEventListener("click", (ev) => {
  if (ev.target === prefsPanel) showPrefs(false);
});
window.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape" && !prefsPanel.hidden) showPrefs(false);
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
  // The panel is standing open in the language it was opened in: the static
  // markup has been redrawn by `setLang`, and these are the lines that are
  // written rather than marked up.
  paintKeptOutput();
  paintCacheUse();
});

/// Tell the cut editor. It reads the store when it opens, so this is only
/// for one that is already up -- and the editor decides for itself what it
/// can act on without being reopened.
function tellEditorPrefs() {
  if (emit) {
    emit("prefs-changed", {
      counter: !!prefs.get("counter"),
      meter: prefs.get("meter") !== false,
      subsOn: !!prefs.get("subsOn"),
      // Not for the editor to store -- both windows read the one store -- but
      // to tell it that the line in its menu is now naming the wrong pass.
      blankShades: String(prefs.get("blankShades") || "both"),
      // ...and that the band under its timeline now ends a picture earlier
      // or later than it was drawn.
      flatMarkAt: String(prefs.get("flatMarkAt") || "after"),
    });
  }
}

el("pref-counter").addEventListener("change", (ev) => {
  prefs.set("counter", ev.target.checked);
  tellEditorPrefs();
});
el("pref-meter").addEventListener("change", (ev) => {
  prefs.set("meter", ev.target.checked);
  tellEditorPrefs();
});
el("pref-subs").addEventListener("change", (ev) => {
  prefs.set("subsOn", ev.target.checked);
  tellEditorPrefs();
});

// How the cut editor answers to the keyboard, and what it does with the mark
// files beside a recording. Nothing to tell that window: both windows are the
// same origin, so it reads these out of the store at the keystroke that needs
// them, and a change here is in force at the next one.
//
// Each of the four is a number and the unit it is counted in, and the pair is
// one answer: 15 is half a second, or the whole timeline going past in seven,
// depending on what stands beside it.
const PAGE_STEPS = [
  ["pref-page-step", "pageStep"],
  ["pref-page-step-shift", "pageStepShift"],
  ["pref-page-step-ctrl", "pageStepCtrl"],
  ["pref-page-step-shift-ctrl", "pageStepShiftCtrl"],
];

/// The two lengths the flat detections judge by, which are the same kind of
/// answer as a step: a number and the unit it is counted in. Handled by the
/// same code below, and listed apart because they are not about the keyboard.
const RUN_LENGTHS = [
  ["pref-blank-run", "blankRun"],
  ["pref-quiet-run", "quietRun"],
];

/// Whether this unit is counted in whole things. Pictures are: half a picture
/// is not a step. A percent is not, being a speed rather than an amount --
/// half a percent a second is a crawl, and somebody watching for a join
/// wants it.
const wholeStep = (unit) => unit === "frame";

/// Fit the field to what it is now counting: whole numbers for pictures,
/// tenths for seconds and for a speed, and nothing over 100 where the number
/// is a share of something.
function paintStepUnit(id, unit) {
  const box = el(id);
  box.step = wholeStep(unit) ? "1" : "0.1";
  if (unit === "pct") box.max = "100";
  else box.removeAttribute("max");
}

/// What a typed number comes to for the unit beside it. Rounded where the
/// unit is counted in whole things, and held at nothing below zero.
const stepValue = (n, unit) => {
  const at = wholeStep(unit) ? Math.round(n) : n;
  return unit === "pct" ? Math.min(at, 100) : at;
};

// A field emptied or typed full of something that is not a number keeps the
// answer it had, and says so by putting it back: a step of NaN is a key that
// silently stops working.
for (const [id, name] of PAGE_STEPS.concat(RUN_LENGTHS)) {
  el(id).addEventListener("change", (ev) => {
    const typed = Number(ev.target.value);
    const kept =
      ev.target.value.trim() !== "" && isFinite(typed) && typed >= 0
        ? stepValue(typed, prefs.get(`${name}Unit`))
        : prefs.get(name);
    prefs.set(name, kept);
    ev.target.value = String(kept);
  });
  // Changing the unit does not convert the number: 15 seconds is not 15
  // percent of anything, and a program that answered a unit change by
  // rewriting the number would be answering a question nobody asked. What it
  // does do is put the number into the shape the new unit is counted in.
  el(`${id}-unit`).addEventListener("change", (ev) => {
    const unit = ev.target.value;
    prefs.set(`${name}Unit`, unit);
    paintStepUnit(id, unit);
    const at = stepValue(Number(prefs.get(name)) || 0, unit);
    prefs.set(name, at);
    el(id).value = String(at);
  });
}

el("pref-sidecar").addEventListener("change", (ev) => {
  prefs.set("sidecarPriority", ev.target.value);
});

// A level rather than a length, so it is one field and no unit. Held to the
// range the field says: a number typed outside it, or nothing at all, keeps
// the answer that was there.
el("pref-quiet-level").addEventListener("change", (ev) => {
  const typed = Number(ev.target.value);
  const kept =
    ev.target.value.trim() !== "" && isFinite(typed) && typed <= 0 && typed >= -90
      ? Math.round(typed)
      : prefs.get("quietLevel");
  prefs.set("quietLevel", kept);
  ev.target.value = String(kept);
});

el("pref-cm-keyframes").addEventListener("change", (ev) => {
  prefs.set("cmKeyframes", ev.target.checked);
});

// Which shades the pictures pass looks for, and whether each of the two
// detections puts its marks down. The shades are a question for the pass
// itself -- a detection saved for black alone is a black detection -- so a
// row that has already been read is read again when this changes; that falls
// out of the cache being asked with the shades in it.
el("pref-cm-inserts").addEventListener("change", (ev) => {
  prefs.set("cmInserts", ev.target.checked);
});
el("pref-flat-mark-at").addEventListener("change", (ev) => {
  prefs.set("flatMarkAt", ev.target.value);
  tellEditorPrefs();
});
/// What the rows are showing about their flat pictures is an answer to a
/// question that has just been withdrawn.
///
/// Forgotten and asked again of the cache with what is now in force. Some of
/// those asks come back with the same answer -- a pass that looked for both
/// shades answers for either on its own -- and some read the recording again,
/// which is the honest outcome: a threshold changes where a fade is *called*
/// black, and that is the frame a mark goes on.
///
/// A row whose pass is booked or running is left where it is. That one is
/// about to write its own answer.
function forgetBlank() {
  for (const c of clips) {
    if (c.blankState !== "done") continue;
    c.blankState = "none";
    c.blankFound = null;
    c.blankPhase = "";
    c.blankSource = null;
    paintRow(c);
    restoreFlat(c);
  }
  paintButtons();
  paintProps();
}

el("pref-blank-shades").addEventListener("change", (ev) => {
  prefs.set("blankShades", ev.target.value);
  // What this window offers, and what the editor's menu offers: both name
  // the pass, and the pass has just been told to look for something else.
  paintBlankLabels();
  tellEditorPrefs();
  forgetBlank();
});

/// How dark is black, how bright is white, and how much of the picture has to
/// be one of them. Percentages on screen and fractions in the engine.
///
/// Each has a floor and a ceiling it is held inside, and an empty field or a
/// word typed into one keeps the answer it had -- the same bargain the step
/// fields make, and for the same reason: a threshold of NaN is a detection
/// that silently finds nothing.
const BLANK_LEVELS = [
  ["pref-blank-black", "blankBlackLevel", 0, 100],
  ["pref-blank-white", "blankWhiteLevel", 0, 100],
  ["pref-blank-coverage", "blankCoverage", 1, 100],
];

for (const [id, name, lo, hi] of BLANK_LEVELS) {
  el(id).addEventListener("change", (ev) => {
    const typed = Number(ev.target.value);
    const kept =
      ev.target.value.trim() !== "" && isFinite(typed)
        ? Math.min(hi, Math.max(lo, Math.round(typed)))
        : prefs.get(name);
    const moved = kept !== prefs.get(name);
    prefs.set(name, kept);
    ev.target.value = String(kept);
    if (moved) forgetBlank();
  });
}

el("pref-blank-keyframes").addEventListener("change", (ev) => {
  prefs.set("blankKeyframes", ev.target.checked);
});

el("pref-quiet-keyframes").addEventListener("change", (ev) => {
  prefs.set("quietKeyframes", ev.target.checked);
});

el("pref-quiet-overwrite").addEventListener("change", (ev) => {
  prefs.set("quietOverwrite", ev.target.checked);
});

// What a cut is called. Written into the settings in force as well as into
// the store: this panel's answers take effect as they are given, and a default
// that would only be seen at the next start is no answer at all to "what is
// this run going to be called". The field on the output settings screen is
// still free to disagree afterwards -- that one is about this project.
el("pref-prefix").addEventListener("input", (ev) => {
  prefs.set("outPrefix", ev.target.value);
  settings.prefix = ev.target.value;
  showSettings();
  touch();
});
el("pref-number").addEventListener("change", (ev) => {
  prefs.set("outNumber", ev.target.checked);
  settings.number = ev.target.checked;
  paintPrefDigits();
  showSettings();
  touch();
});
el("pref-digits").addEventListener("change", (ev) => {
  const digits = Number(ev.target.value) || 2;
  prefs.set("outDigits", digits);
  settings.digits = String(digits);
  showSettings();
  touch();
});

// The sound, which has a second home on the output screen like the three
// above it -- and unlike them it is only in force where the sound is being
// written. Set on a list already open, it lands there straight away; a row
// whose track is narrower than this keeps its own, which is `soundCeiling`'s
// doing and not this one's.
el("pref-audio-channels").addEventListener("change", (ev) => {
  prefs.set("outAudioChannels", ev.target.value);
  settings.audioChannels = ev.target.value;
  // With the mode, for the reason `applyNameDefaults` gives.
  if (ev.target.value) settings.audio = "reencode";
  showSettings();
  touch();
});

// What a cut carries. Unlike the four above it has no second home on the
// output settings screen, so there is nothing to write it into: the next run
// reads it from here. See `prefs.dataBroadcast`.
el("pref-data-broadcast").addEventListener("change", (ev) => {
  prefs.set("dataBroadcast", ev.target.checked);
});

el("pref-keep-output").addEventListener("change", (ev) => {
  prefs.set("keepOutput", ev.target.checked);
  // Turning it on settles what is to be carried at the moment it is turned
  // on, rather than at the next change to a setting: the answer somebody has
  // in front of them is the one they mean.
  if (ev.target.checked) rememberOutput();
});

el("pref-forget-output").addEventListener("click", () => {
  for (const key of Object.keys(SETTING_DEFAULTS)) settings[key] = SETTING_DEFAULTS[key];
  // The program's defaults for everything except what a cut is named, where
  // 環境設定 is the default: it is on this very panel, and a button that put
  // `cut_` back over the answer two rows above it would be arguing.
  applyNameDefaults();
  // The defaults are what nobody has answered, here as at the start: a list
  // put back to them has no output of its own to write down.
  outputSettled = false;
  filledIn = null;
  // Which writes the defaults back over what was being carried: from here on
  // that is what a restart restores, because it is what is now in force.
  showSettings();
  touch();
  note(t("prefs.forgetOutput"));
});

/// The four the engine acts on. Sent as a set rather than one at a time --
/// there is one command and it takes all of them -- and the folder is the
/// only one that can be refused, so it is the only one with anything to say
/// back.
async function pushPrefs() {
  const failed = await prefs.tellBackend(invoke);
  if (failed) note(t("prefs.cacheDirFailed", { e: failed }));
  return !failed;
}

el("pref-clean-joins").addEventListener("change", async (ev) => {
  prefs.set("cleanJoins", ev.target.checked);
  await pushPrefs();
});
el("pref-proxy").addEventListener("change", async (ev) => {
  prefs.set("proxy", ev.target.checked);
  paintProxyWidth();
  await pushPrefs();
});
el("pref-proxy-width").addEventListener("change", async (ev) => {
  prefs.set("proxyWidth", Number(ev.target.value) || 0);
  await pushPrefs();
});
// Seconds, and the engine is told: it is the side that writes the sound.
// A field emptied or filled with something that is not a length keeps the
// answer it had and puts it back, the same as the step fields above.
el("pref-audio-fade").addEventListener("change", async (ev) => {
  const secs = Number(ev.target.value);
  const kept =
    ev.target.value.trim() !== "" && isFinite(secs) && secs >= 0 ? Math.min(secs, 10) : prefs.get("audioFade");
  prefs.set("audioFade", kept);
  ev.target.value = String(kept);
  await pushPrefs();
});

el("pref-ffmpeg-log").addEventListener("change", async (ev) => {
  prefs.set("ffmpegLog", Number(ev.target.value) || 0);
  await pushPrefs();
});

/// Where the backend would put the scratch files if nobody chose. Asked once
/// at startup and shown as the field's placeholder, so that "既定" is a place
/// with a name rather than an empty box.
let cacheHome = "";

el("pref-cache-pick").addEventListener("click", async () => {
  const picked = await dialog.open({ directory: true, multiple: false });
  if (!picked) return;
  const dir = Array.isArray(picked) ? picked[0] : picked;
  const was = prefs.get("cacheDir");
  prefs.set("cacheDir", dir);
  // A folder that cannot be written to is not a preference worth keeping:
  // it would be tried again at every start and fail there too, where there
  // is nobody looking at a panel to be told about it.
  if (!(await pushPrefs())) {
    prefs.set("cacheDir", was);
    await prefs.tellBackend(invoke);
  }
  paintCacheDir();
  paintCacheUse();
});

el("pref-cache-reset").addEventListener("click", async () => {
  prefs.set("cacheDir", "");
  await pushPrefs();
  paintCacheDir();
  paintCacheUse();
});

/// What is on disk, by kind. Three rows, because they do not cost the same
/// to lose: an index is a pass over the recording, a proxy is a whole
/// re-encode of it, and a detection is both.
let cacheTotal = 0;

async function paintCacheUse() {
  const list = el("pref-cache-list");
  const total = el("pref-cache-total");
  let use = null;
  try {
    use = invoke ? await invoke("cache_usage") : null;
  } catch (e) {
    void e;
  }
  list.innerHTML = "";
  cacheTotal = 0;
  if (!use) {
    total.textContent = "";
    el("pref-cache-clear").disabled = true;
    return;
  }
  for (const kind of ["index", "proxy", "cm", "flat"]) {
    const held = use[kind] || { files: 0, bytes: 0 };
    cacheTotal += held.bytes || 0;
    const li = document.createElement("li");
    const what = document.createElement("span");
    what.className = "what";
    what.textContent = t(`prefs.cacheKind.${kind}`);
    const n = document.createElement("span");
    n.className = "num";
    n.textContent = t("prefs.cacheFiles", { n: held.files || 0 });
    const b = document.createElement("span");
    b.className = "num";
    b.textContent = size(held.bytes || 0);
    li.append(what, n, b);
    list.appendChild(li);
  }
  total.textContent = cacheTotal
    ? t("prefs.cacheTotal", { size: size(cacheTotal) })
    : t("prefs.cacheEmpty");
  el("pref-cache-clear").disabled = cacheTotal === 0;
}

el("pref-cache-clear").addEventListener("click", async () => {
  // Asked with the number in it. What is lost is only ever a pass -- the
  // cuts are in the list and in the project file -- and saying so is what
  // makes the question answerable without going to look first.
  const go = await dialog.ask(t("prefs.cacheClearBody", { size: size(cacheTotal) }), {
    title: t("prefs.cacheClearTitle"),
    kind: "warning",
    okLabel: t("prefs.cacheClearOk"),
    cancelLabel: t("prefs.cacheClearCancel"),
  });
  if (!go) return;
  try {
    await invoke("clear_cache");
  } catch (e) {
    note(t("prefs.cacheClearFailed", { e: String(e) }));
  }
  paintCacheUse();
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
/// 終了: the same way out as the window's cross, question and all. In both
/// windows -- the tool is a program somebody leaves running and closes when
/// it is done, and reaching for a menu to do it is no stranger there.
el("menu-quit").addEventListener("click", () => {
  showMenu(false);
  if (invoke) invoke("close_main");
});
el("about-close").addEventListener("click", () => showAbout(false));
about.addEventListener("click", (ev) => {
  if (ev.target === about) showAbout(false);
});
window.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape" && !about.hidden) showAbout(false);
});

/// The name of the pictures pass, wherever this window offers it: the button
/// under クリップ編集 and the line in a row's own menu.
///
/// Written over the markup's `data-i18n` rather than instead of it. That
/// attribute is what `applyStatic` redraws in a new language, and this is
/// what follows 環境設定 -- so this runs after it, here and at every point
/// either of the two answers can have changed.
function paintBlankLabels() {
  const label = t(blankKey("side.detectBlank"));
  setText(el("detect-blank-selected"), label);
  setText(el("row-detect-blank").querySelector("span"), t(blankKey("rowmenu.detectBlank")));
}

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
    // Not while the picture pass is on this row or has just failed on it:
    // the sentence there is that pass's and this one would talk over it.
    if (c.state === "ready" && c.info && c.pics !== "running" && c.pics !== "error") {
      c.phase = indexNote(c);
    }
    if (c.pics === "running") c.phase = t("phase.pictures");
    if (c.pics === "error") c.phase = t("phase.noPictures");
    if (c.cmState === "done" && c.cm && c.cmSource) {
      const note = cmNote(c.cm);
      c.cmPhase = c.cmSource === "cache" ? t("cm.previous", { note }) : note;
    }
    // The two flat detections say the same sentence in either language: a
    // count, and whether it was made in this session. Both are held as
    // numbers and a flag rather than as the sentence, for this.
    for (const which of ["blank", "quiet"]) {
      if (c[`${which}State`] !== "done" || c[`${which}Found`] === null) continue;
      const note = t(`${which}.rowNote`, { n: c[`${which}Found`] });
      c[`${which}Phase`] =
        c[`${which}Source`] === "cache" ? t("flat.previous", { note }) : note;
    }
  }
  renderList();
  renderOutset();
  renderOutScreen();
  renderBatch();
  paintQueueNote();
  paintBlankLabels();
  paintAbout();
  // The editor's window title is this window's doing -- it names the clip,
  // which only the list knows how to name -- so it is this window that has to
  // put it right.
  if (editing) {
    invoke("retitle_editor", { title: t("editor.windowTitle", { clip: clipLabel(editing) }) });
  }
}
onLangChange(relocalise);

// --- keys on the queue --------------------------------------------------
//
// The clip list's keys, on the rows they mean the same thing to. Only in the
// tool, the queue being the only screen it has and no screen the list window
// shows -- and only the four that a queue has an answer to: there is nothing
// here to rename, and nothing to detect.

/// 全選択, which the queue has no button for: the bar is about the queue as a
/// whole and this is about the rows.
function pickAllJobs() {
  batchPicked = new Set(batchJobs.map((j) => j.path));
  batchAnchor = batchJobs.length ? 0 : -1;
  paintPicked();
}

window.addEventListener("keydown", (ev) => {
  if (screen !== "batch") return;
  // A panel is over the queue: the ground behind it says the rest of the
  // program is not listening, and Delete taking a job out from under it would
  // be the queue listening anyway.
  if (!prefsPanel.hidden || !about.hidden) return;
  if (ev.target.tagName === "INPUT" || ev.target.tagName === "SELECT") return;
  const key = ev.key.toLowerCase();
  if ((ev.ctrlKey || ev.metaKey) && key === "a") {
    ev.preventDefault();
    pickAllJobs();
    return;
  }
  if (ev.ctrlKey || ev.metaKey || ev.altKey) return;
  if (ev.key === "Delete" || ev.key === "Backspace") {
    ev.preventDefault();
    dropPicked();
    return;
  }
  // What Enter does to a clip is open it; what it does to a job is open the
  // project the job is, which is the same act one window further out.
  if (ev.key === "Enter") {
    ev.preventDefault();
    openJobProject();
    return;
  }
  if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
    ev.preventDefault();
    if (!batchJobs.length) return;
    const step = ev.key === "ArrowDown" ? 1 : -1;
    const from = batchAnchor < 0 ? (step > 0 ? -1 : batchJobs.length) : batchAnchor;
    const at = clamp(from + step, 0, batchJobs.length - 1);
    pickJob(batchJobs[at], { shiftKey: ev.shiftKey });
    // After the repaint, which has just replaced the row this is about.
    el("batch-list").children[at]?.scrollIntoView({ block: "nearest" });
  }
});

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
  if (!prefsPanel.hidden || !about.hidden) return;
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
  // The other two detections, on the keys the cut editor answers them with:
  // one act, one key, whichever window is in front. B for the black and white
  // pictures, Q for the quiet.
  if ((ev.ctrlKey || ev.metaKey) && key === "b") {
    ev.preventDefault();
    detectFlatSelected("blank");
    return;
  }
  if ((ev.ctrlKey || ev.metaKey) && key === "q") {
    ev.preventDefault();
    detectFlatSelected("quiet");
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
  if (ev.key === "F2") {
    ev.preventDefault();
    renameSelected();
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
noNativeDrag();
applyStatic();
// After `applyStatic`, which has just written the "both" wording into the
// markup: the two names that follow 環境設定 are written over it.
paintBlankLabels();
el("pref-lang").value = preference();
// What 環境設定 says a cut is named, and then the output settings as the last
// session left them where that is what was asked for -- the carried answer is
// the one this session was last used with, so it is the one that wins. Both
// before the first draw rather than after it: the settings screen is drawn from
// `settings`, and putting them back afterwards would show the defaults for as
// long as it takes to redraw.
applyNameDefaults();
restoreOutput();
showSettings();
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
  // And the rest of 環境設定, which the other side acts on: where the scratch
  // files go, whether a proxy is built, how a join is planned. Asked for
  // first and told second -- the four of them also answer to an environment
  // variable, and what that came out as is the default for anything nobody
  // has settled here. Both before the list is touched: a pass queued by
  // `initial_paths` would otherwise run the way the backend was started
  // rather than the way it has been asked.
  .then(() => (invoke ? invoke("prefs_now").catch(() => null) : null))
  .then((now) => {
    if (!now) return null;
    cacheHome = now.cacheHome || "";
    prefs.seed(now);
    return null;
  })
  .then(() => prefs.tellBackend(invoke))
  // Which window this is, and the queue it is to show. Before the list: the
  // tool has no list to open and the two screens where one is built come off
  // its bar, so a window that painted them and took them away a tick later
  // would be a window that flickered into being the wrong program.
  .then(() => settleRole())
  // The one of them that can be refused is the folder for the scratch files,
  // and a folder that was there when it was chosen can be gone by the next
  // start -- an external disk, a share that is not mounted yet. Said on the
  // line the passes report on, because what happens instead is not nothing:
  // the scratch files go back to the place the platform gives.
  .then((failed) => (failed ? note(t("prefs.cacheDirFailed", { e: failed })) : null))
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
    // Straight in, without waiting for the index: the editor builds what it
    // needs itself and shows the recording as it goes. Waiting was for when
    // the list had to have the disc to itself.
    edit(one);
  })
  .catch((e) => jlog(`initial_paths: ${e}`));
