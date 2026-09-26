//! ウインドウの大きさと場所: how each window comes back.
//!
//! Three windows, and no two of them are looked at the same way. The clip
//! list is a list -- as tall as the screen will allow, because what it holds
//! is rows. The editor is a picture with a timeline under it, so it is as
//! wide as the picture wants. The batch tool is a queue somebody leaves in a
//! corner while it runs. A person sizes each of them for what it holds, and
//! puts each of them where it belongs on their screen, and having done it
//! once should not have to do it again.
//!
//! So it is kept per **role** rather than per window label. The batch tool is
//! this same program with `--batch` and its window is the same `main` window
//! (see the バッチ出力 section of `lib.rs`); labelled entries would make the
//! list and the tool one entry, which is the one thing this is for. Roles are
//! the three words `main`, `batch` and `editor`.
//!
//! What is written is the size and place the window would go back to, not the
//! ones it covers: a window left maximized keeps the numbers it had before,
//! and the flag beside them. Restoring the covered size as a plain size is
//! how a window creeps a little larger every time it is opened.
//!
//! A remembered place is the setting that strands a window on a monitor that
//! is no longer there, so no place is used again without asking the screens
//! about it first -- see [`grabbable`]. What is refused falls back to the
//! desktop's own placement, which is what these windows had before any of
//! this: the editor centres itself, and the batch tool asks for the middle
//! once it is up (`center_window`) because the desktop put it half off the
//! bottom.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{LogicalSize, Manager, PhysicalPosition};

use crate::locked;

/// Smaller than this is not a window somebody chose. A desktop mid-animation
/// and a window being folded away both report sizes like it, and either one
/// written down is a program that opens as a postage stamp.
const MIN: f64 = 320.0;

/// A spot on the window's title bar, as far in from its corner as a title bar
/// is deep: a place is worth using again if *this* lands on a screen, because
/// this is the part a person takes hold of. Device pixels, which is what the
/// screens are laid out in.
const GRIP: (i32, i32) = (100, 20);

/// One window as it was left.
#[derive(Serialize, Deserialize, Clone, Copy, Default)]
#[serde(default)]
struct Frame {
    /// The size, in logical pixels -- the same units the configuration states
    /// the defaults in, so that a fresh install and a remembered window mean
    /// the same thing by 1180.
    width: f64,
    height: f64,
    /// The top-left corner of the window, frame and all, in **device** pixels.
    /// A place is a spot on a desktop, and a desktop of two monitors at two
    /// scales has no other units to lay them out in. `None` for a window that
    /// has never been anywhere yet.
    x: Option<i32>,
    y: Option<i32>,
    /// Left maximized. The numbers above are then what it comes back to when
    /// it is un-maximized, not what it covered.
    maximized: bool,
}

impl Frame {
    /// Whether these are numbers worth opening a window at.
    fn sane(&self) -> bool {
        self.width.is_finite()
            && self.height.is_finite()
            && self.width >= MIN
            && self.height >= MIN
    }

    fn place(&self) -> Option<(i32, i32)> {
        self.x.zip(self.y)
    }
}

/// What the file said when this process started. Read once, then only read
/// from: it is the answer to "how does this window open", and that question
/// is asked before the window is there to change it.
static OPENED: Mutex<BTreeMap<String, Frame>> = Mutex::new(BTreeMap::new());

/// A window as it is being watched: what would be written down for it, and
/// where it was one step before that.
///
/// The step back is what a maximize is undone with. Maximizing arrives as a
/// resize to the whole screen and only *then* as the window saying it is
/// maximized, so by the time the flag can be read the covered size has
/// already been measured. Rolling back one step at the flag is what keeps the
/// covered size and the corner of the screen out of the file. See [`note`].
#[derive(Clone, Copy, Default)]
struct Watched {
    frame: Frame,
    prior: Frame,
}

impl Watched {
    /// A window that has not moved yet, starting from what it opened at.
    fn starting(frame: Frame) -> Self {
        Watched { frame, prior: frame }
    }
}

/// What this process has watched happen since. Kept apart from [`OPENED`] so
/// that writing the file back is an overlay of the roles this process
/// actually held windows for: the list window and the batch tool run at the
/// same time over one file, and neither of them may write away the other's
/// entry with a copy it read minutes ago.
static SEEN: Mutex<BTreeMap<String, Watched>> = Mutex::new(BTreeMap::new());

/// The roles whose windows were put back where they were left, this run.
/// [`SEEN`] cannot answer that on its own: the moment a window is watched it
/// knows where it is, whether it was placed there or the desktop was.
static PLACED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Beside the batch queue and the preferences, for the reason given there:
/// this is something its owner settled, not a scratch file.
fn file(app: &tauri::AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_config_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("windows.json"))
}

fn read(path: &Path) -> Option<BTreeMap<String, Frame>> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Take in what was left last time. Called once, before the first window is
/// shown; a file that is not there yet, or that has been edited into
/// nonsense, leaves every window where the configuration and the desktop put
/// it.
pub fn load(app: &tauri::AppHandle) {
    let Some(path) = file(app) else { return };
    if let Some(all) = read(&path) {
        *locked(&OPENED) = all;
    }
}

/// How a role's window was left, if it was.
fn of(role: &str) -> Option<Frame> {
    locked(&OPENED).get(role).copied()
}

/// Open a window as its role was left.
///
/// Called on a window that is not on screen yet -- both windows are built
/// invisible and shown once this has had its say -- because a window that is
/// sized and moved after it is up is a window that visibly jumps as it opens.
///
/// Answers whether the place was used, which is what tells the caller not to
/// centre the window over the top of it.
pub fn restore(window: &tauri::WebviewWindow, role: &str) -> bool {
    let Some(frame) = of(role) else { return false };
    let mut opened = frame;
    // The place first, so that the screen the size is cut down to is the one
    // the window is about to be on rather than the one the desktop guessed.
    let placed = match frame.place() {
        Some((x, y)) if grabbable(window, x, y) => {
            window.set_position(PhysicalPosition::new(x, y)).is_ok()
        }
        _ => false,
    };
    match placed {
        true => {
            locked(&PLACED).insert(role.to_string());
        }
        false => (opened.x, opened.y) = (None, None),
    }
    if frame.sane() {
        let (width, height) = fits(window, frame.width, frame.height);
        let _ = window.set_size(LogicalSize::new(width, height));
        (opened.width, opened.height) = (width, height);
    }
    // What the window opened at is what it is watched from, cut down to the
    // screen and all. Otherwise a size that came from a bigger monitor is
    // written back untouched every time the program is run on the smaller
    // one, and never comes right.
    if opened.sane() || placed {
        locked(&SEEN).insert(role.to_string(), Watched::starting(opened));
    }
    if frame.maximized {
        let _ = window.maximize();
    }
    placed
}

/// Whether a window put here could still be taken hold of.
///
/// A settings folder is carried between machines, a laptop is undocked, a
/// second monitor is unplugged. Any of those leaves a place that is nowhere,
/// and a window at nowhere cannot be dragged back. So the corner of the title
/// bar has to land on one of the screens there are now.
///
/// Screens that cannot be asked about are taken at their word rather than
/// refused: the place was good enough when it was written, and a program that
/// forgets everything on a machine whose monitors it cannot list is worse
/// than one that opens where it was told.
fn grabbable(window: &tauri::WebviewWindow, x: i32, y: i32) -> bool {
    let Ok(screens) = window.available_monitors() else {
        return true;
    };
    if screens.is_empty() {
        return true;
    }
    // Saturating, because the place is read off a file: one edited to the
    // end of the range is a place that is nowhere, not a panic at startup.
    let (grip_x, grip_y) = (x.saturating_add(GRIP.0), y.saturating_add(GRIP.1));
    screens.iter().any(|screen| {
        let (at, size) = (screen.position(), screen.size());
        grip_x >= at.x
            && grip_y >= at.y
            && i64::from(grip_x) < i64::from(at.x) + i64::from(size.width)
            && i64::from(grip_y) < i64::from(at.y) + i64::from(size.height)
    })
}

/// The size cut down to the screen it is opening on.
///
/// The same travelling that [`grabbable`] is for, from the other side: a
/// window sized on a 4K desktop should not open taller than a laptop's screen
/// with its title bar above the top of it.
fn fits(window: &tauri::WebviewWindow, width: f64, height: f64) -> (f64, f64) {
    let Ok(Some(screen)) = window.current_monitor() else {
        return (width, height);
    };
    let scale = screen.scale_factor();
    let size = screen.size();
    (
        width.min(size.width as f64 / scale),
        height.min(size.height as f64 / scale),
    )
}

/// Watch a window for the rest of its life, so that where it was left is
/// known when the program goes.
///
/// A listener of its own rather than a line in the handlers those two windows
/// already have: what is watched for here is neither of the things those were
/// opened for, and where a window is is this module's business wherever the
/// window came from.
pub fn watch(window: &tauri::WebviewWindow, role: &str) {
    // Where it is now, before anything has moved it: a window whose role the
    // file says nothing about has to have somewhere sane to step back to the
    // moment somebody maximizes it.
    note(window, role);
    let app = window.app_handle().clone();
    let label = window.label().to_string();
    let role = role.to_string();
    window.on_window_event(move |event| {
        if !matches!(
            event,
            tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Moved(_)
        ) {
            return;
        }
        // Asked of the window rather than read off the event: what the event
        // carries is one half of the answer, and a maximized window's numbers
        // are not the ones to write down at all. The getters run inline when
        // they are called from the event loop's own thread, which is where
        // this is.
        if let Some(window) = app.get_webview_window(&label) {
            note(&window, &role);
        }
    });
}

/// Write down where a window is at this moment, in memory. The file is
/// written once, on the way out -- see [`save`]. Dragging an edge is a
/// hundred of these.
fn note(window: &tauri::WebviewWindow, role: &str) {
    // Everything the window is asked, asked before the lock is taken. Off the
    // event loop's thread -- `watch` runs on the thread that built the editor,
    // the seam window or 拡大表示 -- each question is a message to that loop
    // and a wait for its answer, and the loop answering a move of another
    // window comes here and waits on this lock: both stopped for good.
    let maximized = window.is_maximized().unwrap_or(false);
    let measured = if maximized { None } else { measure(window) };
    let mut held = locked(&SEEN);
    let mut watched = held
        .get(role)
        .copied()
        .unwrap_or_else(|| Watched::starting(of(role).unwrap_or_default()));
    if maximized {
        // Just gone up. The resize that took it there was measured a moment
        // ago, while the window still said it was not maximized, so what it
        // comes back to is what it was before that.
        if !watched.frame.maximized {
            if watched.prior.sane() {
                watched.frame = watched.prior;
            }
            watched.frame.maximized = true;
        }
    } else {
        watched.frame.maximized = false;
        if let Some(measured) = measured {
            watched.prior = watched.frame;
            watched.frame = measured;
        }
    }
    held.insert(role.to_string(), watched);
}

/// The window as it stands, or nothing if what it says of itself is not worth
/// writing down.
fn measure(window: &tauri::WebviewWindow) -> Option<Frame> {
    let (Ok(inner), Ok(scale)) = (window.inner_size(), window.scale_factor()) else {
        return None;
    };
    let frame = Frame {
        width: inner.width as f64 / scale,
        height: inner.height as f64 / scale,
        // A window on its way to the taskbar is parked far off the desktop --
        // -32000 on Windows -- and that is not a place to open at.
        x: None,
        y: None,
        maximized: false,
    };
    if !frame.sane() {
        return None;
    }
    let at = window.outer_position().ok()?;
    Some(match at.x > -30000 && at.y > -30000 {
        true => Frame {
            x: Some(at.x),
            y: Some(at.y),
            ..frame
        },
        false => frame,
    })
}

/// Put what this process saw into the file, keeping what it did not.
///
/// Read, overlay, rename, in the shape `batch_write` uses over the queue: the
/// other half of the program is another process over this same file, and a
/// half-written settings file is worse than a forgotten window.
pub fn save(app: &tauri::AppHandle) {
    let seen = locked(&SEEN).clone();
    if seen.is_empty() {
        return;
    }
    let Some(path) = file(app) else { return };
    let mut all = read(&path).unwrap_or_default();
    for (role, watched) in seen {
        if watched.frame.sane() || watched.frame.maximized {
            all.insert(role, watched.frame);
        }
    }
    let Ok(text) = serde_json::to_string_pretty(&all) else {
        return;
    };
    // Named for this process: the list window and the batch tool can leave
    // at the same moment, and one shared name had each truncating the
    // other's before the rename.
    let temp = path.with_extension(format!("json.{}.new", std::process::id()));
    if std::fs::write(&temp, text).is_ok() {
        let _ = std::fs::rename(&temp, &path);
    }
}

/// Whether a role's window was put back where it was left, this run.
///
/// For the one caller that would otherwise undo it: the batch tool asks to be
/// centred as it comes up, and a window that has just been put where its
/// owner left it is not a window to move into the middle of the screen.
pub fn placed(role: &str) -> bool {
    locked(&PLACED).contains(role)
}
