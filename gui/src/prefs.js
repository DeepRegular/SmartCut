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
  /// Whether the cut editor draws the subtitles over the picture from the
  /// moment a recording opens, rather than waiting to be asked each time.
  subsOn: false,
  /// Whether it draws the frame number and clock over the picture. Read by
  /// the editor at open; the button on its info bar is the same answer.
  counter: true,
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

/// The four the engine side acts on, in the shape its `set_prefs` wants.
export function forBackend() {
  return {
    cleanJoins: !!get("cleanJoins"),
    proxy: !!get("proxy"),
    proxyWidth: Number(get("proxyWidth")) || 0,
    ffmpegLog: Number(get("ffmpegLog")) || 0,
    cacheDir: String(get("cacheDir") || ""),
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
