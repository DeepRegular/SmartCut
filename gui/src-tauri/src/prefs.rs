//! 環境設定: what the program has been told to do, as against what it has
//! been asked to do.
//!
//! A command carries the work -- this recording, these ranges, that file to
//! write. What is here is the standing answer underneath it: whether a plan
//! may spend re-encoding on a join, whether opening a recording is worth a
//! proxy, where the scratch files go, and how much libav is allowed to say
//! for itself. None of it belongs in a project file, because none of it
//! travels with the work: a project opened on another machine should be cut
//! the same way and cached wherever that machine keeps its caches.
//!
//! Globals, for the reason [`crate::lang`] is one: there is one user looking
//! at one program, and a preference is not a property of a window or of a
//! call. They start at whatever the environment says -- which is how these
//! four were reachable before there was a screen for them -- and the
//! frontend sets them for good the moment it has read its own store.
//!
//! The read side is on the hot paths (`build_plan` runs per clip, the cache
//! directory is asked for on every open), so the four that are numbers or
//! flags are atomics and only the folder takes a lock.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::RwLock;

static CLEAN_JOINS: AtomicBool = AtomicBool::new(false);
static PROXY: AtomicBool = AtomicBool::new(false);
static PROXY_WIDTH: AtomicU32 = AtomicU32::new(0);
static FFMPEG_LOG: AtomicU8 = AtomicU8::new(0);
static CACHE_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

/// How long a plan may spend tidying the start of a range, in seconds.
///
/// The CLI's `--clean-joins` spends the same two seconds. One number in both
/// places on purpose: what the flag does and what the box does have to be
/// the same thing, or a recording cut from the command line and the same one
/// cut from the window are two different outputs.
const JOIN_BUDGET: f64 = 2.0;

/// Anything that reads as yes. The same three spellings every
/// `SMARTCUT_`-prefixed switch has taken since the first one.
fn yes(value: &str) -> bool {
    matches!(value.trim(), "1" | "on" | "yes")
}

/// Take the environment as the starting answer, before the frontend has one.
///
/// Called once, at startup. A machine started with `SMARTCUT_PROXY=1` opens
/// with the box ticked and behaves as it always did; the frontend then sends
/// what is actually stored, which for a preference nobody has settled is
/// what it was told here.
pub fn from_env() {
    if let Ok(v) = std::env::var("SMARTCUT_PROXY") {
        PROXY.store(yes(&v), Ordering::Relaxed);
    }
    if let Some(w) = std::env::var("SMARTCUT_PROXY_WIDTH").ok().and_then(|v| v.trim().parse().ok()) {
        PROXY_WIDTH.store(w, Ordering::Relaxed);
    }
    if let Ok(v) = std::env::var("SMARTCUT_CLEAN_JOINS") {
        CLEAN_JOINS.store(yes(&v), Ordering::Relaxed);
    }
    let level = match std::env::var("SMARTCUT_FFMPEG_LOG").as_deref() {
        Ok("2") | Ok("all") => 2,
        Ok(v) if yes(v) => 1,
        _ => 0,
    };
    FFMPEG_LOG.store(level, Ordering::Relaxed);
    // Applied as well as remembered, for the same reason [`set`] applies it:
    // the level lives inside libav, and a recording named on the command line
    // is opened before the frontend has said anything.
    smartcut_core::set_ffmpeg_log(level);
}

pub fn proxy() -> bool {
    PROXY.load(Ordering::Relaxed)
}

/// The width to build one at, or `None` for the engine's own answer.
pub fn proxy_width() -> Option<u32> {
    let w = PROXY_WIDTH.load(Ordering::Relaxed);
    (w > 0).then_some(w)
}

/// What a plan may spend on a join, in the shape [`smartcut_core::PlanOptions`]
/// wants it: `None` for "do not", rather than a budget of zero.
pub fn clean_join() -> Option<f64> {
    CLEAN_JOINS.load(Ordering::Relaxed).then_some(JOIN_BUDGET)
}

pub fn ffmpeg_log() -> u8 {
    FFMPEG_LOG.load(Ordering::Relaxed)
}

/// Where the scratch files have been told to go, or `None` for the place the
/// platform gives this program.
///
/// A poisoned lock answers `None` rather than panicking: the default cache
/// directory is always a workable answer, and a preference is not worth
/// taking the window down over.
pub fn cache_dir() -> Option<PathBuf> {
    CACHE_DIR.read().ok().and_then(|held| held.clone())
}

/// Settle the four the frontend owns. The folder has already been checked by
/// the caller -- see `set_prefs` -- so what arrives here is a folder that
/// exists and can be written to, or nothing at all.
pub fn set(clean_joins: bool, proxy: bool, proxy_width: u32, ffmpeg_log: u8, dir: Option<PathBuf>) {
    CLEAN_JOINS.store(clean_joins, Ordering::Relaxed);
    PROXY.store(proxy, Ordering::Relaxed);
    PROXY_WIDTH.store(proxy_width, Ordering::Relaxed);
    FFMPEG_LOG.store(ffmpeg_log, Ordering::Relaxed);
    if let Ok(mut held) = CACHE_DIR.write() {
        *held = dir;
    }
    // Applied rather than only remembered: the level libav is set to is
    // global state inside the library, and the point of the setting is the
    // next line it prints.
    smartcut_core::set_ffmpeg_log(ffmpeg_log);
}
