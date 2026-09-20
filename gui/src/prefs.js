// 環境設定: the answers that outlive a project.
//
// A project file carries what was done to one list of recordings -- where
// they are, where the cuts went, what to write out. This carries what was
// settled about the program itself: which of its passes to spend time on,
// where it may put its scratch files, and what a window should look like
// before anybody has touched it. Nothing here belongs in a `.scproj`,
// because none of it travels with the work: a project opened on another
// machine should be cut the same way and cached wherever that machine keeps
// its caches.
//
// The webview's own store, like the language beside it in `i18n.js` and for
// the same reason: both windows are the same origin, so the editor reads
// what the list window wrote without a round trip, and a handful of small
// answers is not worth a file and a command to read it with.
//
// Values are JSON, so a number stays a number and a checkbox stays a
// boolean. Anything unreadable -- a store that will not open, a value from a
// version that meant something else by that name -- comes back as the
// default rather than as an error: a preference is a convenience, and a
// program that will not start because one of them is malformed has got the
// bargain backwards.

/// What each preference is when nobody has said otherwise.
///
/// Four of them -- the proxy, its width, the joins and the FFmpeg log -- also
/// answer to an environment variable, which is how they were reachable
/// before this screen existed. The backend is asked at startup what those
/// came out as and `seed` puts the answers here, so a machine started with
/// `SMARTCUT_PROXY=1` still opens with the box ticked. A preference actually
/// stored beats both.
const DEFAULTS = {
  /// Whether the output settings are put back the way they were left at the
  /// next start. Off, because the settings screen decides what a recording
  /// becomes and a program that quietly remembers yesterday's answer is one
  /// that writes yesterday's file.
  keepOutput: false,
  /// Those settings, as they were last in force. Only read when `keepOutput`
  /// is on; written whenever they change, so that turning the preference on
  /// does not have to wait for the next change to have anything to restore.
  output: null,
  /// Spend up to two seconds of re-encoding at the start of a range to get
  /// off an open GOP cleanly. See `--clean-joins` and `PlanOptions`.
  cleanJoins: false,
  /// Build a proxy when a recording is opened: a whole re-encode, minutes of
  /// every core and gigabytes an hour, for a picture that is cheaper to
  /// scrub. See `proxy_wanted` on the other side.
  proxy: false,
  /// How wide to build it, or 0 for the engine's own answer.
  proxyWidth: 0,
  /// What libav is allowed to print: 0 nothing, 1 warnings, 2 everything.
  ffmpegLog: 0,
  /// How long the sound takes to leave and to come back at a seam, in
  /// seconds. 0 for none.
  ///
  /// A cut joins two instants that were never next to each other, and what
  /// the sound does there is a step. A fade takes the level down into the
  /// join and brings it back, so what is heard is a pause rather than a jump.
  ///
  /// None, because what it fades is the programme: a second either side of
  /// every seam is a second of the recording quieter than it was recorded,
  /// and a cut is meant to be the recording, shorter. Somebody who wants
  /// the join smoothed asks for it and says how much.
  ///
  /// Also answers to `SMARTCUT_AUDIO_FADE`, like the four beside it.
  audioFade: 0,
  /// Where the seek indexes, proxies and detections go, or "" for the place
  /// the platform gives this program. A folder chosen here takes effect at
  /// once and only for what is written from then on: what was already
  /// cached stays where it was written, which is the honest behaviour but
  /// worth saying on screen.
  cacheDir: "",
  /// What a cut's filename begins with, as a project starts out.
  ///
  /// The answer in force belongs to the project -- it is on the output
  /// settings screen and it is written into a `.scproj`, because a list
  /// reopened next year should be written out the way it was settled. This is
  /// what that field holds before anybody has touched it, which is the part
  /// that is about the person rather than about the work: somebody who names
  /// every cut `編集_` should not have to say so again at every start.
  outPrefix: "cut_",
  /// Whether the row's place in the list goes into the name behind the
  /// prefix, and in how many digits.
  ///
  /// The order of a list is an answer somebody gave -- three episodes are in
  /// the order they are watched, two halves of a film in the order they are
  /// played -- and a folder sorted by name is where that answer is otherwise
  /// lost. Same standing as the prefix: the default for a project, which the
  /// output screen can then disagree with.
  ///
  /// On, because the order is nearly always worth keeping and a number in
  /// front of a name that did not need one costs nothing to read past.
  outNumber: true,
  outDigits: 2,
  /// Whether a cut written as a `.ts` carries the recording's data broadcast
  /// -- what is behind the d button.
  ///
  /// Not on the output settings screen and not in a `.scproj`, unlike the
  /// prefix above. It was a per-run box there, and a per-run box is the wrong
  /// shape for this question: it could only appear for a list that held a
  /// recording carrying a carousel and only while a `.ts` was being written,
  /// so somebody who never wants one in their files had to notice the row on
  /// whichever evening it turned up. Asked once, here, it holds for every run.
  ///
  /// On, like the engine's own answer: a cut is meant to be the recording,
  /// shorter, and what is behind the d button was in the recording.
  dataBroadcast: true,
  /// Whether the cut editor draws the subtitles over the picture from the
  /// moment a recording opens, rather than waiting to be asked each time.
  subsOn: false,
  /// How loud the preview plays, 0 to 100, and whether it is silenced.
  ///
  /// Nothing to do with what gets written: the output carries the
  /// recording's own sound whatever these say. They are here rather than in
  /// the project because a checking level belongs to the room the cutting is
  /// being done in, not to the recordings being cut.
  ///
  /// Full, which is the recording as it is. The slider is not a place to
  /// quietly disagree with what was broadcast.
  volume: 100,
  muted: false,
  /// Whether it draws the frame number and clock over the picture. Read by
  /// the editor at open; the button on its info bar is the same answer.
  counter: true,
  /// Whether the cut editor shows the audio level meter beside the picture.
  ///
  /// On. What it answers -- how loud this moment is, and whether the next one
  /// is the silence at a junction -- is half of what a cut is placed by, and
  /// a meter nobody asked for costs 64 pixels of a window that is otherwise
  /// all picture.
  meter: true,
  /// How far 拡大表示 magnifies, in screen pixels per source pixel.
  ///
  /// Not on the 環境設定 screen: it is the one question that window exists to
  /// answer, it is answered in the window itself, and what it should be
  /// changes with what is being looked at. Kept so that a window opened
  /// tomorrow opens where it was left.
  zoomScale: 4,
  /// What PageUp and PageDown do to the playhead: on their own, held with
  /// Shift, held with Ctrl, and held with both.
  ///
  /// Four answers rather than one because they are different questions. A
  /// break is half a minute, a programme is an hour, and checking a join is
  /// a few pictures either side of it -- and which of those a key should do
  /// is not something a program can know about somebody else's recordings.
  /// The arrow keys stay what they are: one picture, and one second with
  /// Shift.
  ///
  /// The unit is part of each answer, and it decides whether the key jumps or
  /// scrolls. `"frame"` counts pictures, which is what a join is looked at
  /// in, and `"sec"` counts time: both are amounts, and the key moves that
  /// far once per press. `"pct"` is a speed: a share of the fastest scroll
  /// the program does, which is sixty times the recording's own speed. A
  /// quarter of it is fifteen times speed, and the number means that whatever
  /// is open. A share of the *timeline* would not: a quarter of an hour's
  /// recording a second is nine hundred times speed, and the same number
  /// would mean something else again on the next recording.
  ///
  /// The numbers are the ones the tool this window is laid out after arrives
  /// with: fifteen pictures and thirty, then a quarter of the top scroll
  /// speed and a half of it, which is fifteen times speed and thirty times.
  /// Two amounts and two speeds, the amounts on the
  /// keys with least held down. Somebody coming from that tool finds these
  /// keys answering the way they are used to, and nobody else has an opinion
  /// about what a page key should do until they open this screen.
  pageStep: 15,
  pageStepUnit: "frame",
  pageStepShift: 30,
  pageStepShiftUnit: "frame",
  pageStepCtrl: 25,
  pageStepCtrlUnit: "pct",
  pageStepShiftCtrl: 50,
  pageStepShiftCtrlUnit: "pct",
  /// Which mark file wins when a recording has more than one beside it.
  ///
  /// `"keyframe"`, `"trim"` or `"cm"`. They do not say the same thing: a
  /// `.keyframe` is a list of places and leaves the timeline whole, a Trim
  /// line is the cut itself and arrives with the material already taken out,
  /// and a saved detection is what a detection made of the recording, band
  /// and marks together. Only asked when more than one is there; any of them
  /// on its own is read whatever this says.
  ///
  /// The marks by default, which is what this program did before it could
  /// read the others: opening a recording to find it already cut is a bigger
  /// thing to do unasked than opening it to find some marks.
  sidecarPriority: "keyframe",
  /// Whether a detection puts its marks down by itself.
  ///
  /// On. A detection that found the breaks and then said nothing about where
  /// they are would be a pass over the recording spent for a sentence, and
  /// putting the marks down is what it has always done. Off is for whoever
  /// wants to look at the band first and decide: the finding is still drawn
  /// under the timeline, and the menu's 「CM 検出結果をキーフレームにする」
  /// puts its marks down when it is asked to.
  ///
  /// About a detection arriving -- one run in here, or one the list hands
  /// over. A finding read out of a file is a mark file being read, and marks
  /// whatever this says. So is the rule this cannot override: marks are never
  /// put on top of a list the recording came up with beside it.
  cmKeyframes: true,
  /// Whether the save shortcut writes over a file that is already there
  /// without stopping to ask.
  ///
  /// Off. The shortcut's whole point is that it does not interrupt, and that
  /// is exactly why the question is worth asking once: the file it writes is
  /// named after the recording, so the one it would land on is always
  /// somebody's earlier answer about the same recording. Whoever saves over
  /// their own work every few minutes can turn this on and stop being asked.
  quietOverwrite: false,
};

const KEY = (name) => `smartcut.${name}`;

/// Seeded defaults, replacing the ones above for anything nobody has stored.
const seeded = {};

/// Take what the backend says is in force for the preferences an environment
/// variable can also set. Only the names it actually answered, and only
/// where nothing is stored: a preference on this side is the user's word,
/// and the environment is what the machine was started with.
export function seed(from) {
  if (!from) return;
  for (const [name, value] of Object.entries(from)) {
    if (name in DEFAULTS && value !== null && value !== undefined) seeded[name] = value;
  }
}

/// Whether this preference has been settled, as opposed to defaulted.
export function stored(name) {
  try {
    return localStorage.getItem(KEY(name)) !== null;
  } catch {
    return false;
  }
}

/// One preference, as it stands.
export function get(name) {
  const fallback = name in seeded ? seeded[name] : DEFAULTS[name];
  let raw;
  try {
    raw = localStorage.getItem(KEY(name));
  } catch {
    return fallback;
  }
  if (raw === null) return fallback;
  try {
    const v = JSON.parse(raw);
    // A stored value of the wrong shape is a value from a version that meant
    // something else by this name. The default is a better answer than a
    // number where a string is expected.
    if (typeof v !== typeof fallback && fallback !== null) return fallback;
    return v;
  } catch {
    // The counter was written as "on"/"off" before any of this existed, and
    // an editor window that has been opened once has one of them in the
    // store. Neither is JSON; both are an answer somebody gave.
    if (raw === "on" || raw === "off") return raw === "on";
    return fallback;
  }
}

/// Settle one. Storing is best effort -- a session whose store will not take
/// it still runs the way it was asked, and only the next one forgets.
export function set(name, value) {
  try {
    localStorage.setItem(KEY(name), JSON.stringify(value));
  } catch {
    // Nothing to say here that the next start will not say for itself.
  }
}

/// Everything, for handing to whatever wants the lot.
export function all() {
  const out = {};
  for (const name of Object.keys(DEFAULTS)) out[name] = get(name);
  return out;
}

/// The six the engine side acts on, in the shape its `set_prefs` wants.
export function forBackend() {
  return {
    cleanJoins: !!get("cleanJoins"),
    proxy: !!get("proxy"),
    proxyWidth: Number(get("proxyWidth")) || 0,
    ffmpegLog: Number(get("ffmpegLog")) || 0,
    cacheDir: String(get("cacheDir") || ""),
    audioFade: Number(get("audioFade")) || 0,
  };
}

/// Tell the backend what it is to do. Awaited by callers with something to
/// start afterwards: a pass that began before this landed would be planned
/// the way the backend was last told rather than the way it has just been
/// asked.
export async function tellBackend(invoke) {
  if (!invoke) return null;
  try {
    return await invoke("set_prefs", { want: forBackend() });
  } catch (e) {
    // An older backend without the command, or a cache folder it will not
    // write to. The first is nothing to report; the second comes back as a
    // sentence, and the screen that asked shows it.
    return String(e);
  }
}
