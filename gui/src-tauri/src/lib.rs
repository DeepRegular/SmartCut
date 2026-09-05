//! Desktop front end for the smart-rendering cutter.
//!
//! The engine does the work; this layer holds the source the editor has
//! open, answers the timeline's questions about it, and runs an export off
//! the UI thread.
//!
//! Beside that sits the clip list's half, which shares none of it. Indexing
//! a clip, detecting its commercials and writing it out all open the
//! recording afresh from the seek index on disc, so they can run while
//! another recording is being edited without ever touching [`Opened`],
//! [`Thumbs`] or [`Proxy`]. The list is the one screen the window can be on
//! while work carries on for a recording nobody is looking at.
//!
//! Those two halves run **at the same time**: an index pass, a commercial
//! detection and an open cut editor, all three at once. Sharing nothing is
//! what makes that safe; what makes it bearable is that the two background
//! passes hold themselves to part of the machine while the editor is up
//! (see [`background_threads`]), because the picture under the pointer is
//! the one somebody is waiting for.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

#[macro_use]
mod lang;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use smartcut_core::{index, netpath, plan, proxy, seek_index, PlanOptions, SeekIndex, Source};
use tauri::{Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

#[derive(Default)]
struct Opened(Mutex<Option<Source>>);

/// The proxy standing in for the recording, once there is one.
///
/// Everything that only wants to *look* at a picture reads from here: the
/// preview, the film strip, playback, the scene search. Everything that has
/// to know about the bitstream -- planning, cutting, commercial detection --
/// keeps reading the recording, because the proxy cannot answer for it.
#[derive(Default)]
struct Proxy(Mutex<Option<Proxied>>);

struct Proxied {
    src: Source,
    /// What kind of picture the *recording* had at each instant. The proxy is
    /// re-encoded, so its own picture types describe the proxy alone.
    marks: proxy::Marks,
}

/// Counted up every time a file is opened. A background pass carries the
/// number it started under and throws its result away if it no longer
/// matches -- otherwise opening a second file while the first is still
/// building would end with one recording's proxy standing in for another's.
#[derive(Default)]
struct Generation(AtomicU64);

/// The thumbnail track and scene index, once the background pass has built
/// them. Kept apart from [`Opened`] so that a pass lasting tens of seconds
/// never holds the lock the timeline needs to answer a keystroke.
#[derive(Default)]
struct Thumbs(Mutex<Option<smartcut_core::Track>>);

/// Which recording [`Thumbs`] holds the track of.
///
/// A lock of its own rather than a read of [`Opened`]: `detect_cm` keeps that
/// one for the whole of a pass that runs into minutes, and the list window
/// asking whether the open track speaks for the row it is drawing must not
/// queue behind it. Written where `Thumbs` is emptied, so the two cannot
/// come apart.
#[derive(Default)]
struct OpenPath(Mutex<Option<String>>);

/// The seek index read off disc for the open recording, when there was one.
///
/// Held between [`open_source`] and [`prepare`] because it answers both: the
/// access points it carries are what the recording was opened with, and the
/// thumbnail track beside them is what `prepare` would otherwise spend a pass
/// over the key pictures building.
#[derive(Default)]
struct Held(Mutex<Option<SeekIndex>>);

/// Set while playback is running; cleared to ask it to stop.
#[derive(Default)]
struct Playing(std::sync::atomic::AtomicBool);

/// How many times each of the clip list's two background lanes has been
/// asked to give up.
///
/// Two counts because there are two lanes -- one seek index and one
/// commercial detection run alongside each other -- and stopping one of them
/// may not touch the other: opening the cut editor on a clip the index lane
/// is reading stops that pass and nothing else.
///
/// A count rather than a flag. A pass takes the number as it starts and
/// gives up as soon as it sees a larger one, so a stop lands on exactly the
/// passes that were running when it was asked for. A flag would have to be
/// lowered again before the next pass could start, and there is no moment to
/// lower it in: the other lane is still watching it.
#[derive(Default)]
struct BatchStop {
    index: AtomicU64,
    cm: AtomicU64,
}

/// One of the recording's sound tracks, as the window needs to know it.
///
/// The list is here because a recording can hold more than one and the window
/// is the only side that knows which of them survives. A pressed disc's
/// Japanese track sits beside an English 5.1 one; libavformat calls the wider
/// of the two the main track, and a count taken from that is the wrong count
/// for a cut that keeps only the other. Named by both index and PID for the
/// same reason [`streams_to_drop`] takes both: the editor answers in indices,
/// the disc chooser answers in PIDs.
#[derive(Serialize, Clone)]
struct AudioTrackInfo {
    index: usize,
    pid: i32,
    /// What the track is, by libav's own name for it -- `aac` off the air,
    /// `truehd` off a disc. The output settings screen sends it back when it
    /// asks what may be written (see [`audio_limits`]): what a track can be
    /// re-encoded into is partly a question of what it already is.
    codec: String,
    channels: u16,
    sample_rate: u32,
    bits: u8,
}

fn audio_tracks_of(src: &Source) -> Vec<AudioTrackInfo> {
    src.audios
        .iter()
        .map(|a| AudioTrackInfo {
            index: a.stream_index,
            pid: a.pid,
            codec: a.codec.clone(),
            channels: a.channels,
            sample_rate: a.sample_rate,
            bits: a.bits,
        })
        .collect()
}

#[derive(Serialize)]
struct SourceInfo {
    path: String,
    codec: String,
    width: u32,
    height: u32,
    fps: f64,
    duration: f64,
    interlaced: bool,
    pulldown: bool,
    has_audio: bool,
    /// Channels in the recording's audio, so the output settings can say
    /// whether there is anything to downmix. 0 when there is no audio.
    audio_channels: u16,
    /// The other two numbers an uncompressed track's size is made of, so the
    /// output settings can multiply them out: linear PCM has no bitrate to
    /// choose, only one to be told. 0 when there is no audio.
    audio_sample_rate: u32,
    audio_bits: u8,
    /// Every sound track, so the window can answer for the one it is keeping
    /// rather than for the one above. See [`AudioTrackInfo`].
    audio_tracks: Vec<AudioTrackInfo>,
    index_name: String,
    /// Presentation times of every random access point: the places a cut
    /// costs nothing.
    points: Vec<f64>,
    /// How many of those cannot start a copy because their leading pictures
    /// are referenced.
    unusable_points: usize,
    /// Where the container's clock begins. Everything else here is already
    /// rebased to it, and this is carried for the one thing that is not: a
    /// time that came from outside the file. A disc's chapter marks are
    /// written on the stream's own clock, and this is what puts them on the
    /// timeline the editor draws.
    start_time: f64,
}

/// One row of the clip list, once the recording behind it has been read.
///
/// A superset of what [`SourceInfo`] carries, bar the access point times --
/// the list wants how many there are, not where they are, and a broadcast
/// recording has tens of thousands of them.
#[derive(Serialize)]
struct ClipInfo {
    path: String,
    name: String,
    codec: String,
    width: u32,
    height: u32,
    fps: f64,
    duration: f64,
    frames: u64,
    interlaced: bool,
    pulldown: bool,
    has_audio: bool,
    audio_channels: u16,
    audio_sample_rate: u32,
    audio_bits: u8,
    /// Every sound track, so the window can answer for the one it is keeping
    /// rather than for the one above. See [`AudioTrackInfo`].
    audio_tracks: Vec<AudioTrackInfo>,
    index_name: String,
    points: usize,
    unusable_points: usize,
    /// Where the material actually begins. Nothing before the first access
    /// point can be decoded and the planner clamps to it, so this is what
    /// the output's own clock starts from -- and broadcast recordings often
    /// open most of a second in.
    first_point: f64,
    scenes: usize,
    /// A picture from a little way in, for the row to show. Absent only when
    /// the pass produced no pictures at all.
    poster: Option<String>,
    /// Whether the seek index was already on disc from an earlier session,
    /// in which case this cost a read and not a pass over the recording.
    cached: bool,
    seconds: f64,
}

/// Serialized both ways: out to the window, and on and off the cache a
/// detection is written to. See [`remember_cm`].
#[derive(Serialize, Deserialize, Clone)]
struct BlockInfo {
    start: f64,
    end: f64,
    junctions: usize,
    score: f64,
}

#[derive(Serialize, Deserialize, Clone)]
struct CmResult {
    logo_found: bool,
    /// How many caption resets were found. Non-zero means the blocks came
    /// from them and neither the audio nor the logo was read.
    resets: usize,
    blocks: Vec<BlockInfo>,
}

#[derive(Serialize)]
struct SegmentInfo {
    kind: String,
    start: f64,
    end: f64,
    frames: usize,
}

#[derive(Serialize)]
struct PlanInfo {
    total: f64,
    copied: f64,
    reencoded: f64,
    segments: Vec<SegmentInfo>,
}

/// The paths given on the command line, so files can be opened without the
/// picker -- handy when the app is launched from a file manager, which hands
/// over everything that was selected.
///
/// Captured before Tauri starts rather than read on demand: nothing should
/// depend on what the process's argv looks like once a webview is up.
struct Argv(Vec<String>);

#[tauri::command]
fn initial_paths(argv: State<Argv>) -> Vec<String> {
    argv.0.clone()
}

/// What one thing the user handed the list turned into.
///
/// One input can be several files: a directory -- a night's recordings on the
/// NAS, named as one share path -- is worth taking whole rather than making
/// the picker walk into it. The frontend keeps its own say over which of the
/// files it will have, so everything readable is listed and nothing is
/// filtered by extension here.
#[derive(Serialize)]
struct Resolved {
    input: String,
    files: Vec<Found>,
    /// Japanese, and shown as it stands: this is the one place the user is
    /// talking about a path they typed, and the reason it did not work is the
    /// whole of the answer.
    error: Option<String>,
}

/// One thing an input named: a recording, or a whole disc.
///
/// A file is only its path: the name in the list is the file's name and a cut
/// of it is written beside it, and there is nothing to say.
///
/// A disc is not one recording but a list of them, and it is not the window's
/// business to decide which of them were meant -- a pressed disc holds twelve
/// episodes among fifty menus, logos and eight second transitions. So it
/// arrives whole, with everything its own index says about each row, and the
/// window asks.
///
/// The two travel in one list rather than two so that a folder of recordings
/// with a disc image sitting in it comes back in the order the folder is in,
/// which is the order somebody named their files to be read in.
#[derive(Serialize)]
#[serde(tag = "what", rename_all = "lowercase")]
enum Found {
    File {
        path: String,
    },
    Disc {
        /// The image or folder itself, which is what the chooser is titled by
        /// and what a second drop of the same disc is recognised by.
        path: String,
        /// `"bdav"` or `"bdmv"`; the window says which kind of disc it is
        /// asking about.
        kind: String,
        /// What the disc calls itself.
        label: String,
        clips: Vec<DiscClip>,
    },
}

impl Found {
    fn file(path: String) -> Found {
        Found::File { path }
    }

    fn disc(at: &std::path::Path, disc: smartcut_core::disc::Disc) -> Found {
        Found::Disc {
            path: at.to_string_lossy().into_owned(),
            kind: disc.shape.as_str().to_string(),
            label: disc.label,
            clips: disc.entries.into_iter().map(DiscClip::from).collect(),
        }
    }
}

/// One row of the chooser: a recording on a disc, and everything the disc's
/// index knows about it.
#[derive(Serialize)]
struct DiscClip {
    /// What to open, which is what everything downstream is keyed on.
    path: String,
    /// What to call it in the list.
    label: String,
    /// What to name a cut of it.
    stem: String,
    /// Where a cut of it goes when no output folder has been chosen. The
    /// folder the disc is in: inside an image there is nowhere to write.
    home: String,
    /// The chapter points the disc wrote, on the stream's own clock.
    ///
    /// The disc's index is the only place they exist -- nothing in the stream
    /// says where a chapter is -- so they are read once, here, and travel
    /// with the row until the editor can put them down as marks.
    chapters: Vec<f64>,
    duration: f64,
    /// How much of the disc it occupies. On a disc whose index names
    /// everything `000NN`, length and size are what tell an episode from a
    /// logo.
    bytes: u64,
    /// Whether to offer it already ticked.
    wanted: bool,
    tracks: Vec<DiscTrack>,
}

impl From<smartcut_core::disc::Entry> for DiscClip {
    fn from(e: smartcut_core::disc::Entry) -> DiscClip {
        DiscClip {
            // Off the title's own clock and onto the clip's, which is the
            // clock the demuxer reports and the only one the editor can
            // rebase.
            chapters: e.marks.iter().map(|m| e.start + m).collect(),
            path: e.path,
            label: e.label,
            stem: e.stem,
            home: e.home,
            duration: e.duration,
            bytes: e.bytes,
            wanted: e.wanted,
            tracks: e.tracks.into_iter().map(DiscTrack::from).collect(),
        }
    }
}

/// One stream a clip carries, as the disc's own index describes it.
///
/// Named by PID and not by a stream index, because a stream index does not
/// exist until something has been opened -- and the whole point of the
/// chooser is to be answerable before anything is. See
/// [`smartcut_core::disc::Track`].
#[derive(Serialize)]
struct DiscTrack {
    kind: String,
    pid: i32,
    detail: String,
    language: Option<String>,
    carried: bool,
}

impl From<smartcut_core::disc::Track> for DiscTrack {
    fn from(t: smartcut_core::disc::Track) -> DiscTrack {
        DiscTrack {
            kind: t.kind.to_string(),
            pid: t.pid,
            detail: t.detail,
            language: t.language,
            carried: t.carried,
        }
    }
}

/// Turn what was dropped, picked or pasted into paths that can be opened.
///
/// `smb://nas/rec/a.ts` and `\\nas\rec\a.ts` become the mount point they are
/// under; an ordinary path passes straight through. Called for every way
/// clips get added, so that a share path works wherever a path works.
#[tauri::command]
fn resolve_paths(paths: Vec<String>) -> Vec<Resolved> {
    paths
        .into_iter()
        .map(|input| match files_at(&input) {
            Ok(files) => Resolved { input, files, error: None },
            Err(error) => Resolved { input, files: Vec::new(), error: Some(error) },
        })
        .collect()
}

/// What one input names: itself, everything in it when it is a directory, or
/// the disc when it is one.
fn files_at(input: &str) -> Result<Vec<Found>, String> {
    let path = local_path(input)?;
    let meta = std::fs::metadata(&path).map_err(|e| {
        let shown = netpath::parse(input).map_or_else(|| path.display().to_string(), |s| s.unc());
        format!("{}: {shown} ({e})", tr!("開けません", "Cannot open"))
    })?;
    // A Blu-ray -- a folder holding one, or an `.iso` of one -- is a list of
    // recordings rather than one recording, and its own index is the only
    // place their names are written down. Tried before the directory walk,
    // because a disc *is* a directory and walking it would find three
    // subdirectories and nothing to open.
    if smartcut_core::disc::looks_like_disc(&path) {
        return match smartcut_core::disc::read(&path) {
            Ok(disc) if !disc.entries.is_empty() => Ok(vec![Found::disc(&path, disc)]),
            // An `.iso` that is not a disc of recordings is not an error
            // worth stopping for when it was one file among a hundred, but
            // it is the whole answer when it is what was dropped.
            Ok(_) | Err(_) if meta.is_dir() => files_in(&path),
            Err(e) => Err(format!(
                "{}: {} ({e})",
                tr!("ディスクを読めません", "Cannot read the disc"),
                path.display()
            )),
            Ok(_) => Err(format!(
                "{}: {}",
                tr!("録画が入っていません", "No recordings on it"),
                path.display()
            )),
        };
    }
    if !meta.is_dir() {
        return Ok(vec![Found::file(path.to_string_lossy().into_owned())]);
    }
    files_in(&path)
}

/// Everything in a directory, in name order -- which for recordings named by
/// date is the order they were made, and in any case an order, which
/// `read_dir` is not.
///
/// A disc inside the folder stands for itself, the same as a disc that was
/// dropped on its own: a folder of `.iso` files, or of copied discs, is one
/// evening's worth of recordings named one way rather than a hundred files
/// named another. Anything else that is a folder is passed over, as it always
/// was.
fn files_in(path: &std::path::Path) -> Result<Vec<Found>, String> {
    let mut names: Vec<std::path::PathBuf> = std::fs::read_dir(path)
        .map_err(|e| {
            format!(
                "{}: {} ({e})",
                tr!("フォルダーを読めません", "Cannot read the folder"),
                path.display()
            )
        })?
        .flatten()
        .map(|e| e.path())
        .collect();
    names.sort();

    let mut out = Vec::new();
    for name in names {
        if smartcut_core::disc::looks_like_disc(&name) {
            if let Ok(disc) = smartcut_core::disc::read(&name) {
                if !disc.entries.is_empty() {
                    out.push(Found::disc(&name, disc));
                    continue;
                }
            }
        }
        if name.is_dir() {
            continue;
        }
        out.push(Found::file(name.to_string_lossy().into_owned()));
    }
    Ok(out)
}

/// The local path for something the user named, translating a share the
/// machine has mounted and saying so plainly when it has not.
fn local_path(input: &str) -> Result<std::path::PathBuf, String> {
    let Some(share) = netpath::parse(input) else {
        return Ok(std::path::PathBuf::from(input));
    };
    netpath::local(&share).map_err(|_| {
        let mounted = netpath::mounts();
        let seen = if mounted.is_empty() {
            String::new()
        } else {
            let list = mounted.iter().map(|m| format!(r"\\{}\{}", m.host, m.share))
                .collect::<Vec<_>>().join("、");
            trf!("（今つながっている共有: {list}）", " (shares connected now: {list})")
        };
        // Named by the share and not by the file: the share is what is not
        // connected, and it is what the file manager is asked to open.
        let root = netpath::Share { rest: String::new(), ..share.clone() };
        let unc = root.unc();
        let url = root.url();
        trf!(
            "{unc} につながっていません。ファイルマネージャーで {url} を開いてから、\
             もう一度追加してください。{seen}",
            "Not connected to {unc}. Open {url} in the file manager and add it again.{seen}",
        )
    })
}

/// Frontend diagnostics, surfaced in the process log. A webview on a
/// headless box has no console anyone can open.
#[tauri::command]
fn log(msg: String) {
    eprintln!("[js] {msg}");
}

/// Run `f` against whatever pictures should be decoded: the proxy's if it is
/// built, the recording's otherwise, and the recording's picture kinds
/// alongside when the two differ.
///
/// Takes one lock at a time, never both. `detect_cm` holds `Opened` for
/// minutes at a stretch, and a preview arriving in the middle of that has to
/// be able to answer from the proxy rather than queue behind it.
fn with_pictures<T>(
    app: &tauri::AppHandle,
    f: impl FnOnce(&Source, Option<&proxy::Marks>) -> Result<T, String>,
) -> Result<T, String> {
    {
        let state = app.state::<Proxy>();
        let guard = state.0.lock().unwrap();
        if let Some(p) = guard.as_ref() {
            return f(&p.src, Some(&p.marks));
        }
    }
    let state = app.state::<Opened>();
    let guard = state.0.lock().unwrap();
    f(guard.as_ref().ok_or("no file open")?, None)
}

/// Run `f` on a worker thread, and hand its answer back when it is done.
///
/// A `#[tauri::command]` that is not `async` runs on the thread the window is
/// drawn on, so anything slow in one is a window that has stopped repainting:
/// the film strip stops following the pointer, the buttons stop lighting up,
/// and the desktop offers to kill the application. Everything that decodes a
/// picture therefore goes through here. It is tens of milliseconds against a
/// proxy, and while a proxy is still being built it is a seek and a GOP out of
/// the recording itself with an encoder already on every core -- which is
/// exactly when the strip was unusable.
async fn off_thread<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn open_source(path: String, app: tauri::AppHandle) -> Result<SourceInfo, String> {
    off_thread(move || open_now(&path, &app)).await
}

/// Where seek indexes are kept.
fn index_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| {
            format!("{}: {e}", tr!("キャッシュの置き場が分かりません", "No cache directory"))
        })?
        .join("index");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// The seek index written for this recording by an earlier session, if there
/// is one and it can still be read.
///
/// A file that cannot be read is deleted rather than stepped around: it would
/// be found again on the next open and fail again.
fn held_index(app: &tauri::AppHandle, path: &str) -> Option<SeekIndex> {
    let file = seek_index::cache_path(&index_dir(app).ok()?, path).ok()?;
    if !file.is_file() {
        return None;
    }
    match SeekIndex::load(&file) {
        Ok(ix) => {
            // Not the least recently used, since it is being used now.
            seek_index::touch(&file);
            Some(ix)
        }
        Err(e) => {
            eprintln!("index: discarding {}: {e}", file.display());
            let _ = std::fs::remove_file(&file);
            None
        }
    }
}

/// Open `path` with a previous session's seek index where one still fits,
/// and hand the index back alongside so its thumbnail track can be picked up.
///
/// Touches nothing shared. The clip list works through recordings in the
/// background while another one is being edited, and neither pass may stand
/// on the other's [`Opened`], [`Thumbs`] or [`Held`].
fn scan_cached(app: &tauri::AppHandle, path: &str) -> Result<(Source, Option<SeekIndex>), String> {
    // The pass over the packets is the same answer every time, so a previous
    // session's is taken where there is one: about a second per gigabyte
    // saved, which on a half-hour recording is the whole of the wait between
    // choosing a file and being able to move the pointer.
    let mut held = held_index(app, path);
    let mut src = match &held {
        Some(ix) => smartcut_core::scan_with(path, ix),
        None => smartcut_core::scan(path),
    };
    // An index that the key could not tell was stale is still an index that
    // does not fit. Out it goes, and this open reads the file.
    if src.is_err() && held.is_some() {
        eprintln!("index: {} -- reading the recording instead", src.unwrap_err());
        if let Ok(dir) = index_dir(app) {
            if let Ok(file) = seek_index::cache_path(&dir, path) {
                let _ = std::fs::remove_file(file);
            }
        }
        held = None;
        src = smartcut_core::scan(path);
    }
    Ok((src.map_err(|e| e.to_string())?, held))
}

fn info_of(src: &Source) -> SourceInfo {
    SourceInfo {
        path: src.path.clone(),
        codec: src.video.codec.clone(),
        width: src.video.width,
        height: src.video.height,
        fps: src.video.frame_rate,
        duration: src.duration,
        interlaced: src.video.interlaced(),
        pulldown: src.video.pulldown,
        has_audio: src.audio.is_some(),
        audio_channels: src.audio.as_ref().map_or(0, |a| a.channels),
        audio_sample_rate: src.audio.as_ref().map_or(0, |a| a.sample_rate),
        audio_bits: src.audio.as_ref().map_or(0, |a| a.bits),
        audio_tracks: audio_tracks_of(src),
        index_name: src.index_name.to_string(),
        points: src.points.iter().map(|p| p.time).collect(),
        unusable_points: src.points.iter().filter(|p| p.open_gop() && !p.droppable).count(),
        start_time: src.start_time,
    }
}

/// Reading a recording's index is a pass over the file, so it runs on a
/// worker thread; see [`off_thread`].
fn open_now(path: &str, app: &tauri::AppHandle) -> Result<SourceInfo, String> {
    let (src, held) = scan_cached(app, path)?;
    // Anything still building for the file that was open belongs to nothing
    // now; the count going up is what tells it so.
    app.state::<Generation>().0.fetch_add(1, Ordering::SeqCst);
    *app.state::<Thumbs>().0.lock().unwrap() = None;
    // Named by what was asked for rather than by what came back: this is
    // compared against a path the list window holds, and that is the string
    // it handed over.
    *app.state::<OpenPath>().0.lock().unwrap() = Some(path.to_string());
    *app.state::<Proxy>().0.lock().unwrap() = None;
    *app.state::<Held>().0.lock().unwrap() = held;
    let info = info_of(&src);
    *app.state::<Opened>().0.lock().unwrap() = Some(src);
    Ok(info)
}

#[derive(Serialize)]
struct Shot {
    url: String,
    time: f64,
    /// "I", "P" or "B". An I picture is where a cut costs nothing.
    kind: String,
}

fn as_url(jpeg: &[u8]) -> String {
    format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(jpeg))
}

#[tauri::command]
async fn preview(time: f64, width: u32, app: tauri::AppHandle) -> Result<Shot, String> {
    off_thread(move || {
        with_pictures(&app, |src, marks| {
            let s = smartcut_core::shot_at(src, time, width).map_err(|e| e.to_string())?;
            Ok(Shot { url: as_url(&s.jpeg), time: s.time, kind: kind_of(&s, src, marks) })
        })
    })
    .await
}

/// What kind of picture this is, as the *recording* has it.
///
/// A proxy is re-encoded, so its own I and P pictures fall where its encoder
/// put them; the letter beside the frame number is about the recording, and
/// so is the "無劣化点" beside it.
fn kind_of(shot: &smartcut_core::Shot, src: &Source, marks: Option<&proxy::Marks>) -> String {
    marks
        .and_then(|m| m.kind_at(shot.time, src.video.frame_duration() / 2.0))
        .unwrap_or(shot.kind)
        .to_string()
}

/// Pictures for the film strip, at exactly the times asked for.
///
/// The strip walks the *edited* timeline, so neighbouring cells can sit
/// either side of a cut. Asking by time rather than by "centre and spacing"
/// is what lets the caller hand over a run that jumps.
#[tauri::command]
async fn thumbs_at(
    times: Vec<f64>,
    width: u32,
    exact: Option<bool>,
    app: tauri::AppHandle,
) -> Result<Vec<Option<Shot>>, String> {
    if times.is_empty() {
        return Ok(Vec::new());
    }
    off_thread(move || thumbs_now(&times, width, exact, &app)).await
}

fn thumbs_now(
    times: &[f64],
    width: u32,
    exact: Option<bool>,
    app: &tauri::AppHandle,
) -> Result<Vec<Option<Shot>>, String> {
    let thumbs = app.state::<Thumbs>();
    with_pictures(app, |src, marks| {
        // The spacing asked for says whether the held pictures are fine enough
        // to answer with: they sit one key picture apart. `exact` overrides
        // that -- a caller asking about a cut's join needs the picture *at* the
        // time it named, because the nearest held one may be the last picture the
        // cut took away.
        //
        // The median gap rather than the smallest, and weighed against the
        // track's own median. The film strip's GOP mode asks at the recording's
        // own entry points, and those are not evenly spaced: one short GOP
        // anywhere in the window would take the smallest gap below the track's
        // spacing and send the whole strip off to be decoded -- a second and a
        // half for sixty cells -- when every time it asked for is a picture
        // already in hand.
        let gaps: Vec<f64> = times
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .filter(|d| *d > 1e-9)
            .collect();
        let gap = smartcut_core::thumbs::median_gap(&gaps).unwrap_or(f64::INFINITY);
        // A held picture sits *on* a key picture, and every time asked for down
        // this path is a key picture's own, so the nearest held picture has to
        // *be* the one asked for. Further off than that means there is a hole in
        // the track -- an entry point that arrived damaged, say, or one thinned
        // out by the cap -- and answering with whatever is nearest would put a
        // caption from one time under a picture from another. Those slots are
        // left for a real decode below.
        //
        // Two nominal frames of slack, and not the track's own spacing: under
        // 2:3 pulldown a picture runs to three fields, so one nominal frame is
        // not quite enough. The spacing was what this used to allow, which made
        // the tolerance a hundredth of a second on a three-minute recording and
        // half a second on a half-hour one -- the same hole caught on one and
        // papered over on the other, for no reason to do with either.
        let held: Option<Vec<Option<Shot>>> = {
            let guard = thumbs.0.lock().unwrap();
            guard
                .as_ref()
                .filter(|t| !exact.unwrap_or(false) && gap >= t.interval * 0.9)
                .map(|track| {
                    let tol = src.video.frame_duration() * 2.0;
                    times
                        .iter()
                        .map(|&t| {
                            track.nearest(t).filter(|h| (h.time - t).abs() <= tol).map(|h| Shot {
                                url: as_url(&h.jpeg),
                                time: h.time,
                                kind: "I".into(),
                            })
                        })
                        .collect()
                })
        };
        if let Some(mut out) = held {
            let holes: Vec<usize> = (0..out.len()).filter(|&i| out[i].is_none()).collect();
            if !holes.is_empty() {
                let want: Vec<f64> = holes.iter().map(|&i| times[i]).collect();
                let shots = smartcut_core::shots_at(src, &want, width).map_err(|e| e.to_string())?;
                for (&i, shot) in holes.iter().zip(shots) {
                    out[i] = shot.map(|s| Shot {
                        url: as_url(&s.jpeg),
                        time: s.time,
                        kind: kind_of(&s, src, marks),
                    });
                }
            }
            return Ok(out);
        }

        let shots = smartcut_core::shots_at(src, times, width).map_err(|e| e.to_string())?;
        Ok(shots
            .into_iter()
            .map(|o| {
                o.map(|s| Shot { url: as_url(&s.jpeg), time: s.time, kind: kind_of(&s, src, marks) })
            })
            .collect())
    })
}

#[derive(Serialize)]
struct TrackInfo {
    thumbs: usize,
    interval: f64,
    scenes: Vec<f64>,
    threshold: f64,
    typical: f64,
    seconds: f64,
}

#[derive(Serialize)]
struct ProxyInfo {
    path: String,
    /// What the proxy turned out to be encoded as. Reported rather than
    /// chosen: which encoder took it depends on what the machine has.
    codec: String,
    width: u32,
    height: u32,
    bytes: u64,
    /// Whether one was already on disc from a previous session.
    cached: bool,
    seconds: f64,
}

#[derive(Serialize)]
struct IndexInfo {
    path: String,
    bytes: u64,
    /// Whether it was already on disc from a previous session.
    cached: bool,
}

#[derive(Serialize)]
struct PrepareInfo {
    proxy: Option<ProxyInfo>,
    /// The seek index this open left behind, or picked up. Absent only when
    /// the cache could not be written at all.
    index: Option<IndexInfo>,
    track: TrackInfo,
    /// Why there is no proxy, when one was asked for and could not be made.
    /// Empty otherwise -- including in the ordinary case where none was
    /// asked for, which is not a failure and must not read as one.
    note: String,
}

fn track_info(track: &smartcut_core::Track, seconds: f64) -> TrackInfo {
    TrackInfo {
        thumbs: track.thumbs.len(),
        interval: track.interval,
        scenes: track.scenes.clone(),
        threshold: track.threshold,
        typical: track.typical,
        seconds,
    }
}

/// Is the proxy asked for?
///
/// Off unless it is. A proxy is a whole re-encode of the recording -- a
/// minute or two of every core, and about four gigabytes an hour of
/// programme -- and what it buys is the *picture* getting cheaper to decode.
/// The seek index buys the rest of it for a fraction of a percent of that:
/// the pass over the packets and the pass over the key pictures are both kept
/// instead of repeated, and the byte offsets it carries take the guesswork
/// out of seeking. What is left for the proxy is material where decoding one
/// picture is itself too slow to scrub -- which 1440x1080 MPEG-2 is not, and
/// 8K will be.
fn proxy_wanted() -> bool {
    matches!(std::env::var("SMARTCUT_PROXY").as_deref(), Ok("1") | Ok("on") | Ok("yes"))
}

/// Make the recording ready to look at: the thumbnail track and scene index,
/// and the proxy if one was asked for.
#[tauri::command]
async fn prepare(app: tauri::AppHandle) -> Result<PrepareInfo, String> {
    let (src, generation) = {
        let state = app.state::<Opened>();
        let guard = state.0.lock().unwrap();
        let src = guard.as_ref().ok_or("no file open")?.clone();
        (src, app.state::<Generation>().0.load(Ordering::SeqCst))
    };
    tauri::async_runtime::spawn_blocking(move || {
        let thumb_opts = smartcut_core::ThumbOptions::default();
        let mut note = String::new();
        if proxy_wanted() {
            let opts = proxy::ProxyOptions::default();
            match make_proxy(&app, &src, &opts, &thumb_opts, generation) {
                Ok(Some(info)) => return Ok(info),
                // Superseded: another file was opened while this ran.
                Ok(None) => return Err("cancelled".to_string()),
                Err(why) => {
                    // A build that was abandoned because another file was
                    // opened is not a failure to fall back from: the
                    // recording it was for is not the one on screen any more.
                    if app.state::<Generation>().0.load(Ordering::SeqCst) != generation {
                        return Err("cancelled".to_string());
                    }
                    eprintln!("proxy: {why}");
                    note = why;
                }
            }
        }
        without_proxy(&app, &src, &thumb_opts, generation, note)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The ordinary path: the recording answers for its own pictures, and what is
/// kept about it is the seek index.
///
/// Either the index from an earlier session already holds the thumbnail
/// track, in which case there is nothing to do at all, or the key pictures
/// are decoded once and the result written down so that this is the last time
/// -- about ten seconds for half an hour of 1440x1080 MPEG-2, against the
/// minute or two a proxy takes over the same material.
fn without_proxy(
    app: &tauri::AppHandle,
    src: &Source,
    thumb_opts: &smartcut_core::ThumbOptions,
    generation: u64,
    note: String,
) -> Result<PrepareInfo, String> {
    let began = std::time::Instant::now();
    let current = || app.state::<Generation>().0.load(Ordering::SeqCst);

    // The index read when the file was opened carries the track it was built
    // with. Taken rather than borrowed: the pictures are tens of megabytes
    // and there is no reason to hold them twice.
    let kept = app.state::<Held>().0.lock().unwrap().as_mut().and_then(|ix| ix.track.take());
    if let Some(track) = kept {
        let info = track_info(&track, began.elapsed().as_secs_f64());
        if current() != generation {
            return Err("cancelled".to_string());
        }
        let index = index_info(app, src, true);
        *app.state::<Thumbs>().0.lock().unwrap() = Some(track);
        return Ok(PrepareInfo { proxy: None, index, track: info, note });
    }

    let reporter = app.clone();
    let sharer = app.clone();
    let watcher = app.clone();
    let mut track = smartcut_core::thumbs::build_with(
        src,
        thumb_opts,
        Some(Box::new(move |f| {
            let _ = reporter.emit("prepare-progress", (phase_index(), f));
        })),
        Some(Box::new(move |batch| hold(&sharer, batch, generation))),
        Some(Box::new(move || {
            watcher.state::<Generation>().0.load(Ordering::SeqCst) != generation
        })),
    )
    // Superseded reads as cancelled, not as a failure: the file this pass
    // was for is not the one on screen any more, and the one that is has a
    // pass of its own running.
    .map_err(|e| if current() != generation { "cancelled".to_string() } else { e.to_string() })?;
    if current() != generation {
        return Err("cancelled".to_string());
    }
    // The pictures handed over during the pass are already held; what came
    // back has the tail of them and the scene index. Both halves are this
    // pass's, so they go back together in the order they were made.
    if let Some(head) = app.state::<Thumbs>().0.lock().unwrap().take() {
        let tail = std::mem::take(&mut track.thumbs);
        track.thumbs = head.thumbs;
        track.thumbs.extend(tail);
    }
    let info = track_info(&track, began.elapsed().as_secs_f64());
    let index = remember(app, src, Some(&track));
    *app.state::<Thumbs>().0.lock().unwrap() = Some(track);
    Ok(PrepareInfo { proxy: None, index, track: info, note })
}

/// Write down what this open worked out, so the next one does not repeat it.
///
/// A failure here is not one worth stopping for: the recording is open and
/// everything works, it will simply cost the same passes again next time.
fn remember(
    app: &tauri::AppHandle,
    src: &Source,
    track: Option<&smartcut_core::Track>,
) -> Option<IndexInfo> {
    let dir = index_dir(app).ok()?;
    let file = seek_index::cache_path(&dir, &src.path).ok()?;
    if let Err(e) = SeekIndex::of(src, track).save(&file) {
        eprintln!("index: cannot write {}: {e}", file.display());
        return None;
    }
    // An index is a thousandth of what a proxy costs -- the pictures are 192
    // pixels wide and there is no video at all, so half an hour comes to
    // around forty megabytes -- which is why the count is large where the
    // proxy cache's is eight. A gigabyte holds a couple of dozen recordings,
    // and the index for one finished last week is still worth having.
    let _ = seek_index::prune(&dir, 32, 1 << 30);
    index_info(app, src, false)
}

fn index_info(app: &tauri::AppHandle, src: &Source, cached: bool) -> Option<IndexInfo> {
    let file = seek_index::cache_path(&index_dir(app).ok()?, &src.path).ok()?;
    Some(IndexInfo {
        bytes: std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0),
        path: file.to_string_lossy().into_owned(),
        cached,
    })
}

/// Open the cut editor, in its own window.
///
/// A window rather than a fourth tab. Cutting is the one thing here that is
/// *done to* a clip rather than settled once for the list, and the reference
/// tool makes the same split: its cut editor is a dialog you come back out
/// of with OK, not a page you leave by clicking elsewhere. Keeping it in the
/// tab bar meant the same window had to be both the list and the thing being
/// edited, and there was no moment that said "done with this one".
///
/// The window is made here rather than from the page so that its size and
/// title are not the webview's business, and so no capability has to be
/// opened up for building windows out of JavaScript.
///
/// `async` and not for any awaiting it does -- there is none. Tauri runs a
/// synchronous command on the main thread, and building a webview there is
/// the one thing WebView2 will not do: the creation waits on the message
/// loop that the command is standing in, and on Windows the window comes up
/// and stays blank (wry#583). An async command is spawned off the main
/// thread instead, which is what Tauri's own doc for the builder tells you
/// to do. It costs nothing on Linux -- `build` hands itself back to the main
/// thread there either way -- so the two platforms take the same path.
#[tauri::command]
async fn open_editor(title: String, app: tauri::AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(EDITOR) {
        // Already up: this is a second double-click, not a second editor.
        let _ = w.set_title(&title);
        let _ = w.unminimize();
        let _ = w.set_focus();
        return Ok(());
    }
    let window = WebviewWindowBuilder::new(&app, EDITOR, WebviewUrl::App("editor.html".into()))
        .title(title)
        .inner_size(1240.0, 860.0)
        .min_inner_size(900.0, 620.0)
        .center()
        .build()
        .map_err(|e| e.to_string())?;
    // The list has to know the editor has gone, whichever way it went -- OK,
    // キャンセル, or the title bar's cross. Said from here rather than from the
    // page, because the page going away is the thing being reported.
    let teller = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            let _ = teller.emit("editor-closed", ());
        }
    });
    Ok(())
}

/// Retitle the editor window without touching it otherwise.
///
/// [`open_editor`] would do this too, but it also raises and focuses the
/// window, which is right for a second double-click and wrong for the one
/// caller here: the title carries the word カット編集 in it, so it goes stale
/// when the language changes, and correcting it is not a reason to pull the
/// window to the front of somebody's screen.
#[tauri::command]
fn retitle_editor(title: String, app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(EDITOR) {
        let _ = w.set_title(&title);
    }
}

#[tauri::command]
fn close_editor(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(EDITOR) {
        let _ = w.close();
    }
}

/// Name the open project in the list window's own title bar.
///
/// The one thing about a project that ought to be readable without opening a
/// menu: which one is open. Work that has never been saved leaves the plain
/// program name, which is what the window is called in the configuration.
#[tauri::command]
fn retitle_main(title: String, app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN) {
        let _ = w.set_title(&title);
    }
}

/// The pass both windows call the seek index, said in one place because both
/// of them print it and a phase named two ways is two phases on screen.
fn phase_index() -> &'static str {
    tr!("シーク用インデックス", "the seek index")
}

/// The label the editor window goes by. One at a time: there is one opened
/// recording in [`Opened`], so a second editor would be a second view of the
/// first one's material with the first one's marks.
const EDITOR: &str = "editor";

/// The clip list, which Tauri labels for us: a window declared in the
/// configuration without a label of its own is `main`.
const MAIN: &str = "main";

/// The last path component, which is what the list shows.
fn clip_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// Ask the clip list's background passes to stop: `"index"`, `"cm"`, or
/// both when no lane is named.
///
/// It says nothing about what comes after. A pass that has not started yet
/// takes the count as it finds it, so there is nothing to undo and no
/// "resume" to call -- whether more work is taken is the list's own
/// business, not a flag held down here.
#[tauri::command]
fn stop_batch(lane: Option<String>, stop: State<BatchStop>) {
    match lane.as_deref() {
        Some("index") => stop.index.fetch_add(1, Ordering::SeqCst),
        Some("cm") => stop.cm.fetch_add(1, Ordering::SeqCst),
        _ => {
            stop.index.fetch_add(1, Ordering::SeqCst);
            stop.cm.fetch_add(1, Ordering::SeqCst)
        }
    };
}

/// How many cores one of the clip list's background passes may decode with.
///
/// Every core while nobody is cutting anything: the list working through a
/// night's recordings on its own should have the machine, and the two lanes
/// splitting it between them is the whole of the sharing needed.
///
/// Half of it, split again between the lanes, while the cut editor is open.
/// A pass over a recording is a decoder on every core, and the film strip
/// asking for the picture under the pointer is one more decode that has to
/// come back inside a frame or two. This is the trade that lets all three
/// run at once: the pass takes a few seconds longer, the pointer keeps its
/// picture. Zero is what libavcodec reads as "as many as this machine has".
fn background_threads(app: &tauri::AppHandle) -> usize {
    if app.get_webview_window(EDITOR).is_none() {
        return 0;
    }
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    (cores / (2 * LANES)).max(1)
}

/// How many background passes the clip list runs at once: an index and a
/// commercial detection. See [`BatchStop`].
const LANES: usize = 2;

/// How far into a clip its poster is taken from.
const POSTER_AT: f64 = 0.1;

/// The picture a row shows for a clip, out of the track already built for it.
///
/// A little way in rather than at the head, because broadcast recordings open
/// on black or on the tail of the programme before -- and a little way into
/// *what survives* rather than into the file, because a tenth of the way into
/// a recording is as likely as not inside the first commercial break, which
/// is exactly the stretch the cuts took out. A row showing the advertisement
/// it was cut to remove is the one picture that cannot be right.
///
/// `keeps` is what survives, in source time: the same ranges the plan is made
/// from. The instant is worked out on the output clock and put back on the
/// recording's, and the held picture nearest it is then taken from inside the
/// same stretch -- [`Track::nearest`] alone would happily answer with the one
/// on the far side of a join, which is a frame the finished file does not
/// contain.
fn poster_of<'a>(
    track: &'a smartcut_core::Track,
    keeps: &[(f64, f64)],
) -> Option<&'a smartcut_core::thumbs::Thumb> {
    let kept: f64 = keeps.iter().map(|(a, b)| (b - a).max(0.0)).sum();
    if kept <= 0.0 {
        return None;
    }
    let mut want = kept * POSTER_AT;
    let mut at = keeps[0].0;
    for &(a, b) in keeps {
        let len = (b - a).max(0.0);
        at = a + want.min(len);
        if want < len {
            break;
        }
        want -= len;
    }
    // A track still being built speaks only for what it has decoded, and
    // `nearest` answers past that with its last picture -- the wrong picture,
    // where none is the honest answer and the caller can look elsewhere.
    if at > track.covered + 1e-6 {
        return None;
    }
    track
        .thumbs
        .iter()
        .filter(|t| keeps.iter().any(|&(a, b)| t.time >= a - 1e-6 && t.time < b))
        .min_by(|x, y| (x.time - at).abs().total_cmp(&(y.time - at).abs()))
        // Keeps shorter than the spacing of the held pictures: there is no
        // frame of what survives to show, so the nearest one there is stands
        // for the clip rather than the row going blank.
        .or_else(|| track.nearest(at))
}

/// Read one clip of the list and leave its seek index on disc.
///
/// The same work [`prepare`] does for the recording being edited, done ahead
/// of time and for a recording nothing is looking at: the pass over the
/// packets and the pass over the key pictures, written down under the same
/// cache key. So opening this clip for editing later finds both already made
/// and returns at once.
///
/// Deliberately shares nothing with the editing session -- no [`Opened`], no
/// [`Thumbs`], no [`Generation`] -- because it runs while another recording
/// is open and being cut.
#[tauri::command]
async fn index_clip(path: String, app: tauri::AppHandle) -> Result<ClipInfo, String> {
    off_thread(move || index_clip_now(&path, &app)).await
}

fn index_clip_now(path: &str, app: &tauri::AppHandle) -> Result<ClipInfo, String> {
    let began = std::time::Instant::now();
    // What the lane had been asked to stop before this pass existed is not
    // about this pass. See [`BatchStop`].
    let mine = app.state::<BatchStop>().index.load(Ordering::SeqCst);
    let stopped = move || app.state::<BatchStop>().index.load(Ordering::SeqCst) != mine;
    let say = |phase: &str, done: f64| {
        let _ = app.emit("clip-progress", (path.to_string(), phase.to_string(), done));
    };
    if stopped() {
        return Err("cancelled".into());
    }
    say(tr!("読み込み中", "Reading"), 0.0);
    let (src, held) = scan_cached(app, path)?;

    // An index from an earlier session carries the pictures it was built
    // with, so there is nothing left to do for this clip at all.
    let kept = held.and_then(|mut ix| ix.track.take());
    let (track, cached) = match kept {
        Some(track) => (track, true),
        None => {
            let reporter = app.clone();
            let owned = path.to_string();
            let watcher = app.clone();
            let track = smartcut_core::thumbs::build_with(
                &src,
                &smartcut_core::ThumbOptions {
                    threads: background_threads(app),
                    ..Default::default()
                },
                Some(Box::new(move |f| {
                    let _ = reporter.emit(
                        "clip-progress",
                        (owned.clone(), phase_index().to_string(), f),
                    );
                })),
                // Nothing is looking at this recording, so there is nobody to
                // hand pictures to as they are made.
                None,
                Some(Box::new(move || {
                    watcher.state::<BatchStop>().index.load(Ordering::SeqCst) != mine
                })),
            )
            // A pass that was asked to stop does not come back with what it
            // had; it gives up where it stands and says so. Said in the one
            // word the list watches for, because a stop is not a failure --
            // the row goes back to 解析待ち rather than red.
            .map_err(|e| if stopped() { "cancelled".to_string() } else { e.to_string() })?;
            // And a pass that was asked between finishing and returning is
            // the same thing. Writing its track down would leave an index
            // claiming to speak for the whole file, and every later session
            // would believe it.
            if stopped() {
                return Err("cancelled".into());
            }
            remember(app, &src, Some(&track));
            (track, false)
        }
    };

    // The whole of the recording is what this row is so far: a clip that
    // arrived with cuts already on it -- out of a project file -- asks for
    // its picture again through [`clip_poster`] once it has one to ask with.
    let first_point = src.points.first().map_or(0.0, |p| p.time);
    let poster = poster_of(&track, &[(first_point, src.duration)])
        .or_else(|| track.thumbs.first())
        .map(|t| as_url(&t.jpeg));
    say(tr!("完了", "Done"), 1.0);
    Ok(ClipInfo {
        name: clip_name(path),
        path: src.path.clone(),
        codec: src.video.codec.clone(),
        width: src.video.width,
        height: src.video.height,
        fps: src.video.frame_rate,
        duration: src.duration,
        frames: (src.duration * src.video.frame_rate).round().max(0.0) as u64,
        interlaced: src.video.interlaced(),
        pulldown: src.video.pulldown,
        has_audio: src.audio.is_some(),
        audio_channels: src.audio.as_ref().map_or(0, |a| a.channels),
        audio_sample_rate: src.audio.as_ref().map_or(0, |a| a.sample_rate),
        audio_bits: src.audio.as_ref().map_or(0, |a| a.bits),
        audio_tracks: audio_tracks_of(&src),
        index_name: src.index_name.to_string(),
        points: src.points.len(),
        unusable_points: src.points.iter().filter(|p| p.open_gop() && !p.droppable).count(),
        first_point,
        scenes: track.scenes.len(),
        poster,
        cached,
        seconds: began.elapsed().as_secs_f64(),
    })
}

/// The proxy half of [`prepare`]. `Ok(None)` means another file was opened
/// while this was running and the result belongs to nobody.
fn make_proxy(
    app: &tauri::AppHandle,
    src: &Source,
    opts: &proxy::ProxyOptions,
    thumb_opts: &smartcut_core::ThumbOptions,
    generation: u64,
) -> Result<Option<PrepareInfo>, String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| {
            format!("{}: {e}", tr!("キャッシュの置き場が分かりません", "No cache directory"))
        })?
        .join("proxy");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = proxy::cache_path(&dir, &src.path, opts).map_err(|e| e.to_string())?;
    let began = std::time::Instant::now();

    let held = proxy::ready(&path).then(|| load_cached(app, &path, thumb_opts)).and_then(|r| {
        r.map_err(|e| {
            // A proxy that cannot be read is worse than no proxy: it would be
            // found again on the next open and fail again. Out it goes, and
            // this open builds a new one.
            eprintln!("proxy: discarding {}: {e}", path.display());
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(proxy::marks_path(&path));
        })
        .ok()
    });

    let (psrc, marks, track, cached) = if let Some((psrc, marks, track)) = held {
        (psrc, marks, track, true)
    } else {
        let reporter = app.clone();
        let sharer = app.clone();
        let watcher = app.clone();
        let mut built = proxy::build(
            src,
            &path.to_string_lossy(),
            opts,
            thumb_opts,
            Some(Box::new(move |f| {
                let _ = reporter.emit("prepare-progress", (tr!("プロキシ", "the proxy"), f));
            })),
            Some(Box::new(move |batch| hold(&sharer, batch, generation))),
            Some(Box::new(move || {
                watcher.state::<Generation>().0.load(Ordering::SeqCst) != generation
            })),
        )
        .map_err(|e| e.to_string())?;
        // The pictures handed over during the build are already held; what
        // came back has the tail of them and the scene index. Both halves are
        // this build's, so they go back together in the order they were made.
        if app.state::<Generation>().0.load(Ordering::SeqCst) == generation {
            if let Some(head) = app.state::<Thumbs>().0.lock().unwrap().take() {
                let tail = std::mem::take(&mut built.track.thumbs);
                built.track.thumbs = head.thumbs;
                built.track.thumbs.extend(tail);
            }
        }
        let psrc = proxy::open_with(&built.path, built.marks.times.first().copied())
            .map_err(|e| e.to_string())?;
        // Old proxies are only worth what the recordings they stand for are;
        // a handful is enough to keep the files worked on lately instant.
        // Eight recordings back, or four gigabytes, whichever runs out first.
        // A proxy is worth keeping -- reopening a recording that has one
        // costs nothing -- but not at the price of a disk, and at the width
        // and quality the picture needs these run about four gigabytes an
        // hour of programme (measured: a half-hour recording builds a 2.3 GB
        // proxy at the 1280 default). Four gigabytes is two half-hour shows.
        //
        // The budget has to clear one proxy on its own or the cache holds
        // nothing: `prune` never deletes the newest, so a budget below the
        // size of a single file would delete every other one the moment it
        // was written, and reopening yesterday's recording would rebuild it
        // from the recording every time. That is what two gigabytes became
        // when the width went from 960 to 1280.
        let _ = proxy::prune(&dir, 8, 4 << 30);
        (psrc, built.marks, built.track, false)
    };

    let info = ProxyInfo {
        path: path.to_string_lossy().into_owned(),
        codec: psrc.video.codec.clone(),
        width: psrc.video.width,
        height: psrc.video.height,
        bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        cached,
        seconds: began.elapsed().as_secs_f64(),
    };
    let tinfo = track_info(&track, info.seconds);

    if app.state::<Generation>().0.load(Ordering::SeqCst) != generation {
        return Ok(None);
    }
    // The access points are the recording's whichever file the pictures came
    // from, and a proxy's thumbnails sit on the same key pictures as the
    // recording's -- that is what makes the two interchangeable. So the seek
    // index is written here too, and a later open with the proxy turned back
    // off still finds it.
    let index = remember(app, src, Some(&track));
    // Whatever a held index was carrying, the proxy's own track is what the
    // timeline will use. Dropped rather than left to sit: it is tens of
    // megabytes of pictures nothing will ask for again.
    *app.state::<Held>().0.lock().unwrap() = None;
    *app.state::<Thumbs>().0.lock().unwrap() = Some(track);
    *app.state::<Proxy>().0.lock().unwrap() = Some(Proxied { src: psrc, marks });
    Ok(Some(PrepareInfo { proxy: Some(info), index, track: tinfo, note: String::new() }))
}

/// Hold on to thumbnails a build has just produced, so that everything which
/// reads held pictures can start using them now rather than when the build
/// ends.
///
/// The pass runs for ten seconds over half an hour of recording, and for a
/// minute or two where a proxy is being built as well, and until it finished
/// there was nothing to answer the film strip, the scroll search or the mark
/// cards with but the recording itself -- a seek and a GOP each time. These
/// are the same pictures that pass is decoding anyway, and it decodes them in
/// order from the start, so the part of the recording already gone past
/// answers instantly while the rest of it is still being read.
fn hold(app: &tauri::AppHandle, batch: smartcut_core::thumbs::Batch, generation: u64) {
    if app.state::<Generation>().0.load(Ordering::SeqCst) != generation {
        return;
    }
    let (count, interval, covered) = (batch.thumbs.len(), batch.interval, batch.covered);
    {
        let state = app.state::<Thumbs>();
        let mut guard = state.0.lock().unwrap();
        match guard.as_mut() {
            Some(track) => {
                track.thumbs.extend(batch.thumbs);
                track.covered = batch.covered;
                // The spacing is measured, so every batch knows it a little
                // better than the one before -- and it decides whether the
                // film strip may answer from these pictures at all.
                track.interval = batch.interval;
            }
            None => *guard = Some(batch.into_track()),
        }
    }
    if count > 0 {
        let _ = app.emit("prepare-held", (interval, covered));
    }
}

/// Pick up the proxy built for this recording last time, and make the
/// thumbnail track again from it.
///
/// The track is images, tens of megabytes of them, and reading it back off
/// disc would be most of the cost of making it -- so it is not kept. Making
/// it again is a pass over the small file rather than over the recording.
fn load_cached(
    app: &tauri::AppHandle,
    path: &std::path::Path,
    thumb_opts: &smartcut_core::ThumbOptions,
) -> Result<(Source, proxy::Marks, smartcut_core::Track), String> {
    let marks = proxy::Marks::load(&proxy::marks_path(path)).map_err(|e| e.to_string())?;
    let psrc = proxy::open_with(&path.to_string_lossy(), marks.times.first().copied())
        .map_err(|e| e.to_string())?;
    // Kept from being pruned as the least recently used, since it plainly is
    // not: it is being used now.
    touch(path);
    let reporter = app.clone();
    let track = smartcut_core::thumbs::build(
        &psrc,
        thumb_opts,
        Some(Box::new(move |f| {
            let _ = reporter.emit("prepare-progress", (tr!("サムネイル", "thumbnails"), f));
        })),
    )
    .map_err(|e| e.to_string())?;
    Ok((psrc, marks, track))
}

/// Mark a file as used just now, so the least-recently-used pruning is about
/// use and not about when the file happened to be written.
fn touch(path: &std::path::Path) {
    let _ = std::fs::OpenOptions::new().write(true).open(path).and_then(|f| {
        f.set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now()))
    });
}

/// The held picture nearest `time`. Returns nothing rather than decoding on
/// demand: this answers a pointer moving across the scrubber, and a decode
/// would take longer than the pointer stays anywhere.
#[tauri::command]
fn hover_thumb(time: f64, thumbs: State<Thumbs>) -> Option<Shot> {
    let guard = thumbs.0.lock().unwrap();
    // `nearest` will hand back the last picture it has for any time past the
    // end of a track that is still being built -- the wrong picture, where
    // none is the honest answer.
    let t = guard.as_ref().filter(|t| time <= t.covered)?.nearest(time)?;
    Some(Shot { url: as_url(&t.jpeg), time: t.time, kind: "I".into() })
}

/// The next cut in `dir`, refined from the key picture that reported it to
/// the frame the picture actually changes on.
///
/// Refining always moves the answer *earlier* -- the key picture is the first
/// one to show the new scene, and the cut itself is somewhere in the GOP
/// before it. So a refined answer can land back behind where the search
/// started, and pressing the button again would sit on the same scene
/// forever. Each candidate is therefore checked for having actually moved,
/// and the next one taken if it has not.
#[tauri::command]
async fn scene_search(from: f64, dir: i32, app: tauri::AppHandle) -> Result<Option<f64>, String> {
    // Refining a boundary decodes the GOP it falls in, once per candidate;
    // see [`off_thread`].
    off_thread(move || scene_now(from, dir, &app)).await
}

fn scene_now(from: f64, dir: i32, app: &tauri::AppHandle) -> Result<Option<f64>, String> {
    let thumbs = app.state::<Thumbs>();
    with_pictures(app, |src, _| {
        let fd = src.video.frame_duration();
        let mut at = from;
        for _ in 0..8 {
            let coarse = {
                let guard = thumbs.0.lock().unwrap();
                let track = guard.as_ref().ok_or("scene index not built yet")?;
                if dir >= 0 {
                    track.scene_after(at)
                } else {
                    track.scene_before(at)
                }
            };
            let Some(coarse) = coarse else { return Ok(None) };
            at = coarse;
            let exact = smartcut_core::thumbs::refine(src, coarse).map_err(|e| e.to_string())?;
            let moved =
                if dir >= 0 { exact > from + fd / 2.0 } else { exact < from - fd / 2.0 };
            if moved {
                return Ok(Some(exact));
            }
        }
        Ok(None)
    })
}

fn build_plan(src: &Source, ranges: &[(f64, f64)]) -> Vec<smartcut_core::RangePlan> {
    plan(&src.video, src.duration, &src.points, ranges, &PlanOptions::default())
}

#[tauri::command]
async fn make_plan(ranges: Vec<(f64, f64)>, app: tauri::AppHandle) -> Result<PlanInfo, String> {
    // The first plan for a recording may have to read its leading pictures,
    // which is a decode; see [`off_thread`].
    off_thread(move || plan_now(&ranges, &app)).await
}

fn plan_now(ranges: &[(f64, f64)], app: &tauri::AppHandle) -> Result<PlanInfo, String> {
    let state = app.state::<Opened>();
    let mut guard = state.0.lock().unwrap();
    let src = guard.as_mut().ok_or("no file open")?;
    if !src.leading_known {
        index::refine_leading(
            &src.path.clone(),
            &src.video.clone(),
            src.start_time,
            &mut src.points,
            ranges,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(plan_info(&build_plan(src, ranges)))
}

fn plan_info(plans: &[smartcut_core::RangePlan]) -> PlanInfo {
    let copied: f64 = plans.iter().map(|p| p.copied()).sum();
    let reencoded: f64 = plans.iter().map(|p| p.reencoded()).sum();
    PlanInfo {
        total: copied + reencoded,
        copied,
        reencoded,
        segments: plans
            .iter()
            .flat_map(|p| &p.segments)
            .map(|s| SegmentInfo {
                kind: s.kind.as_str().to_string(),
                start: s.start,
                end: s.end,
                frames: s.frames,
            })
            .collect(),
    }
}

/// The cutting plan for a clip of the list, without opening it.
///
/// Same answer [`make_plan`] gives for the recording in the editor, for one
/// that is not in it. The output screen wants it to say which parts of a
/// clip will be re-encoded before it starts writing -- and after the editor
/// became its own window, the list is a place you can stand with no
/// recording open at all.
#[tauri::command]
async fn clip_plan(
    path: String,
    ranges: Vec<(f64, f64)>,
    app: tauri::AppHandle,
) -> Result<PlanInfo, String> {
    off_thread(move || {
        let (mut src, _) = scan_cached(&app, &path)?;
        if !src.leading_known {
            index::refine_leading(
                &src.path.clone(),
                &src.video.clone(),
                src.start_time,
                &mut src.points,
                &ranges,
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(plan_info(&build_plan(&src, &ranges)))
    })
    .await
}

/// One stream of a recording, as the track menu lists it.
#[derive(Serialize)]
struct StreamInfo {
    /// What names it to the engine. `drop_streams` is a list of these.
    index: usize,
    /// "audio", "caption", or "dropped" for what a cut cannot carry.
    kind: String,
    /// The PID it arrived on, which is what a broadcast names it by and what
    /// the output puts it back on.
    pid: i32,
    language: Option<String>,
    /// Filled in for sound: the codec, the rate and the channel count.
    detail: String,
    /// Whether this is the track everything that reads one track reads.
    main: bool,
    /// Whether it can be switched off at all. A recording's only sound track
    /// can be, its data broadcast cannot -- that one is listed to say it is
    /// going, not to offer a choice about it.
    optional: bool,
}

/// What a recording carries, for the track menu to lay out.
///
/// Reads the cached scan, so opening the menu on a clip the list has already
/// read costs nothing. What it does not do is decide anything: which of these
/// are written is the window's answer, sent back with the export.
#[tauri::command]
async fn tracks(path: String, app: tauri::AppHandle) -> Result<Vec<StreamInfo>, String> {
    off_thread(move || {
        let (src, _) = scan_cached(&app, &path)?;
        let main = src.audio.as_ref().map(|a| a.stream_index);
        let mut out = Vec::new();
        for a in &src.audios {
            out.push(StreamInfo {
                index: a.stream_index,
                kind: "audio".into(),
                pid: a.pid,
                language: a.language.clone(),
                detail: format!("{} {}Hz {}ch", a.codec, a.sample_rate, a.channels),
                main: main == Some(a.stream_index),
                optional: true,
            });
        }
        for c in &src.captions {
            out.push(StreamInfo {
                index: c.stream_index,
                kind: "caption".into(),
                pid: c.pid,
                language: c.language.clone(),
                detail: "ARIB STD-B24".into(),
                main: false,
                optional: true,
            });
        }
        // Listed but not offered: see `StreamInfo::optional`.
        for d in &src.dropped {
            out.push(StreamInfo {
                index: usize::MAX,
                kind: "dropped".into(),
                pid: d.pid,
                language: None,
                detail: d.what.to_string(),
                main: false,
                optional: false,
            });
        }
        Ok(out)
    })
    .await
}

/// Pictures out of a clip of the list, at the instants asked for.
///
/// One open for the lot: the seek index makes opening cheap but not free,
/// and these are asked for in small batches -- the frames either side of
/// each join, which is what the output screen puts on show.
#[tauri::command]
async fn clip_thumbs(
    path: String,
    times: Vec<f64>,
    width: u32,
    app: tauri::AppHandle,
) -> Result<Vec<Option<Shot>>, String> {
    off_thread(move || {
        let (src, _) = scan_cached(&app, &path)?;
        Ok(times
            .into_iter()
            .map(|t| {
                smartcut_core::shot_at(&src, t, width)
                    .ok()
                    .map(|s| Shot { url: as_url(&s.jpeg), time: s.time, kind: s.kind.to_string() })
            })
            .collect())
    })
    .await
}

/// A picture for a row that has not been read yet.
///
/// The index pass is what gives a row its picture, and until it has run there
/// is nothing to show: a folder of twenty recordings dropped on the window is
/// twenty blank rows, and the last of them stays blank for as long as the
/// nineteen ahead of it take. This is the same row's picture arrived at the
/// cheap way -- one seek and one GOP, tens of milliseconds -- so that the list
/// looks like what was dropped on it while the passes get on with the reading.
///
/// Taken a tenth of the way in, which is where the real poster comes from, so
/// that the picture the row settles on is the one it started with. `None`
/// rather than an error when nothing decodes: the row is no worse off than it
/// was, and the pass behind this one will say what is actually wrong with the
/// recording.
#[tauri::command]
async fn clip_glance(path: String) -> Result<Option<String>, String> {
    off_thread(move || {
        let width = smartcut_core::ThumbOptions::default().width;
        match smartcut_core::glance(&path, POSTER_AT, width) {
            Ok(jpeg) => Ok(Some(as_url(&jpeg))),
            Err(e) => {
                eprintln!("glance: {path}: {e}");
                Ok(None)
            }
        }
    })
    .await
}

/// The row's picture again, for a clip whose cuts have changed what it is.
///
/// `keeps` is what survives, in source time. Nothing is decoded: the answer
/// comes out of the track the editor already has where this is the clip open
/// in it -- which is the case that matters, since the row repaints as the cut
/// is being made -- and out of the seek index on disc otherwise. `None` where
/// there is neither, and the row keeps the picture it had.
#[tauri::command]
async fn clip_poster(
    path: String,
    keeps: Vec<(f64, f64)>,
    app: tauri::AppHandle,
) -> Result<Option<String>, String> {
    off_thread(move || {
        let open_here = {
            let state = app.state::<OpenPath>();
            let guard = state.0.lock().unwrap();
            guard.as_deref() == Some(path.as_str())
        };
        if open_here {
            let state = app.state::<Thumbs>();
            let guard = state.0.lock().unwrap();
            let url = guard.as_ref().and_then(|t| poster_of(t, &keeps)).map(|t| as_url(&t.jpeg));
            if url.is_some() {
                return Ok(url);
            }
        }
        // Not the scan: only the pictures are wanted, and an index whose key
        // no longer fits the recording is not found at all.
        let Some(track) = held_index(&app, &path).and_then(|mut ix| ix.track.take()) else {
            return Ok(None);
        };
        Ok(poster_of(&track, &keeps).map(|t| as_url(&t.jpeg)))
    })
    .await
}

/// Where a long pass reports the phase it is in and how far through it is.
///
/// Shared rather than borrowed because each of the three passes wants its own
/// copy to carry off onto a worker thread.
type Say = std::sync::Arc<dyn Fn(&str, f64) + Send + Sync>;

/// Look for commercial breaks: caption resets where the broadcaster sends
/// them, otherwise runs of short silences spaced on a 15-second grid.
///
/// `src` is what the bitstream is read out of -- captions, audio, logo --
/// and `pictures` what a boundary is refined against, which is the proxy
/// where there is one and `src` itself otherwise. `say` carries the phase
/// and how far through it back to whoever asked, because the same pass
/// serves the editor's own button and the clip list's batch, and the two
/// report to different places.
fn detect_now(src: &Source, pictures: &Source, say: Say) -> Result<CmResult, String> {
    let opts = smartcut_core::DetectOptions::default();

    // Reading the audio is a few seconds; the logo is two passes over the
    // video and takes ten times as long. Weighting them that way is what
    // makes the bar move at an honest rate rather than sitting at 10%.
    //
    // The caption stream goes first: where the broadcaster resets the
    // service at its junctions, those marks are exact and cost one pass
    // over a stream nothing has to decode. It is also the only signal of
    // the three that is cheap enough to try speculatively.
    const CAPTION_SHARE: f64 = 0.15;
    let reporter = say.clone();
    let resets = smartcut_core::caption::resets_with(
        src,
        Some(Box::new(move |f| {
            (*reporter)(tr!("字幕を調べています", "Reading the captions"), f * CAPTION_SHARE)
        })),
    )
    .ok();

    // With the resets in hand neither of the other two reads anything:
    // the audio is a few seconds, but the logo is two passes over the
    // video, and it is the weaker signal wherever the marks exist.
    let rest = 1.0 - CAPTION_SHARE;
    let audio_share = 0.1 * rest;
    let silences = match &resets {
        Some(_) => Vec::new(),
        None => {
            let reporter = say.clone();
            smartcut_core::find_silences_with(
                src,
                &opts,
                Some(Box::new(move |f| {
                    (*reporter)(
                        tr!("音声を調べています", "Reading the audio"),
                        CAPTION_SHARE + f * audio_share,
                    )
                })),
            )
            .map_err(|e| e.to_string())?
        }
    };
    let cands = smartcut_core::cm_candidates(&silences, &opts);

    // The logo is the better read on how far a break runs, but not every
    // broadcaster shows one; when it is missing, the silences stand alone.
    let logo = if resets.is_none() {
        let reporter = say.clone();
        smartcut_core::logo::detect_with(
            src,
            &Default::default(),
            Some(Box::new(move |f| {
                (*reporter)(
                    tr!("ロゴを探しています", "Looking for the logo"),
                    CAPTION_SHARE + audio_share + f * (rest - audio_share),
                )
            })),
        )
        .ok()
    } else {
        None
    };
    (*say)(tr!("まとめています", "Putting it together"), 1.0);
    let blocks = match (&resets, &logo) {
        (Some(r), _) => smartcut_core::cm_blocks_from_resets(r, src.duration),
        (None, Some(l)) if !l.absent.is_empty() => {
            smartcut_core::cm_blocks_from_logo(&cands, &l.absent, &opts, 3.0, src.duration)
        }
        (None, Some(_)) => Vec::new(),
        _ => smartcut_core::cm_blocks(&cands, &opts, 0.6),
    };
    // The times a block arrives with are estimates -- the middle of a
    // silence, or the moment a logo's rolling average crossed a threshold.
    // Neither is a picture. The cut itself is a scene change, so a
    // boundary within reach of one is moved onto the exact frame it
    // happens on.
    let mut blocks = blocks;
    smartcut_core::cm_refine_boundaries(pictures, &mut blocks, 0.5, 0.08);

    Ok(CmResult {
        logo_found: logo.is_some(),
        resets: resets.as_ref().map_or(0, |r| r.len()),
        blocks: blocks
            .into_iter()
            .map(|b| BlockInfo {
                start: b.start,
                end: b.end,
                junctions: b.junctions,
                score: b.score,
            })
            .collect(),
    })
}

// --- remembering a detection --------------------------------------------
//
// A detection is minutes of reading the recording, and it is the same answer
// every time: the same file gives the same caption resets, the same silences,
// the same logo. Yet until now it lived only in the window that asked for it,
// so a list built again the next morning showed nothing about recordings that
// had been detected the night before -- and re-detecting them was the whole
// evening again.
//
// Kept beside the seek index and the proxy, in the cache directory rather
// than beside the recording: the recordings sit on a share that other things
// read, and a file this program wrote for its own convenience does not belong
// in with them. Losing the cache costs a pass, never any work of the user's --
// what they cut is in the clip list and in the `.keyframe` beside the output.

/// Where detections are kept.
fn cm_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| {
            format!("{}: {e}", tr!("キャッシュの置き場が分かりません", "No cache directory"))
        })?
        .join("cm");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Bumped when what is written here stops meaning what it used to -- a change
/// to the detection that would make yesterday's answer the wrong one. Every
/// cached detection is then ignored, and the recordings are read again.
const CM_VERSION: u32 = 1;

/// Where this recording's detection belongs.
///
/// Keyed the way [`seek_index::cache_path`] and [`proxy::cache_path`] key
/// theirs, and for the same reason: the path alone would go on answering for
/// a recording that has since been replaced, so the size and the modification
/// time are in the key and a changed file simply misses.
fn cm_path(app: &tauri::AppHandle, src_path: &str) -> Result<std::path::PathBuf, String> {
    // Asked of the file the recording is in, which for one on a disc is the
    // disc: a path into an image is not a path the operating system knows.
    // The key is still the recording's own name, so two on one disc do not
    // share a detection.
    let file = smartcut_core::input::Input::parse(src_path).map_err(|e| e.to_string())?.file;
    let meta = std::fs::metadata(file).map_err(|e| e.to_string())?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    // FNV-1a, as in the other two.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x1_0000_01b3);
        }
    };
    eat(src_path.as_bytes());
    eat(&meta.len().to_le_bytes());
    eat(&mtime.to_le_bytes());
    eat(&CM_VERSION.to_le_bytes());

    let stem: String = std::path::Path::new(src_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .take(40)
        .collect();
    Ok(cm_dir(app)?.join(format!("{stem}-{h:016x}.cmj")))
}

/// Write a detection down, so the next session's list already knows.
///
/// A failure is not worth stopping for: the answer is in the window either
/// way, and all that is lost is having it again tomorrow.
fn remember_cm(app: &tauri::AppHandle, src_path: &str, res: &CmResult) {
    let Ok(file) = cm_path(app, src_path) else { return };
    match serde_json::to_vec(res) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&file, json) {
                eprintln!("cm: cannot write {}: {e}", file.display());
                return;
            }
        }
        Err(e) => {
            eprintln!("cm: cannot encode: {e}");
            return;
        }
    }
    // A few hundred bytes each -- a block is four numbers and there are
    // rarely more than a dozen -- so the limit is about not leaving an
    // unbounded directory behind rather than about the space. A thousand
    // recordings is more than anyone's list has held.
    if let Ok(dir) = cm_dir(app) {
        let _ = cm_prune(&dir, 1000);
    }
}

/// A detection an earlier session wrote for this recording, if the file is
/// still the one it was written for.
///
/// One that cannot be read is deleted rather than stepped around, as with the
/// seek index: it would be found again next time and fail again.
fn cached_cm(app: &tauri::AppHandle, src_path: &str) -> Option<CmResult> {
    let file = cm_path(app, src_path).ok()?;
    let raw = std::fs::read(&file).ok()?;
    match serde_json::from_slice::<CmResult>(&raw) {
        Ok(res) => {
            // Not the least recently used, since it is being used now.
            seek_index::touch(&file);
            Some(res)
        }
        Err(e) => {
            eprintln!("cm: discarding {}: {e}", file.display());
            let _ = std::fs::remove_file(&file);
            None
        }
    }
}

/// Delete the least recently used detections until at most `keep` remain.
///
/// Same shape as [`seek_index::prune`] without its byte budget, which would
/// be measuring nothing: these are text files a page long.
fn cm_prune(dir: &std::path::Path, keep: usize) -> std::io::Result<()> {
    let mut found: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("cmj") {
            continue;
        }
        let when = path
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        found.push((when, path));
    }
    if found.len() <= keep {
        return Ok(());
    }
    found.sort_by_key(|(when, _)| std::cmp::Reverse(*when));
    for (_, path) in found.into_iter().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

/// What is already known about `path`'s commercials, for a clip the list has
/// just been handed.
///
/// Answers with nothing where nothing has been detected, which is the usual
/// case and costs a `stat` and a miss.
#[tauri::command]
async fn cm_cached(path: String, app: tauri::AppHandle) -> Result<Option<CmResult>, String> {
    // Reads two small files at most, but one of them is on whatever the
    // recording is on -- a share that has gone to sleep answers a `stat` in
    // its own time, and the list adds clips a hundred at a go.
    //
    // The path is taken as it is given, not put through `local_path`: it has
    // already been through `resolve_paths`, and it is the string the
    // detection will be written under, so the two have to agree.
    off_thread(move || Ok(cached_cm(&app, &path))).await
}

/// The editor's own detection, against the recording that is open.
#[tauri::command]
async fn detect_cm(app: tauri::AppHandle) -> Result<CmResult, String> {
    // Reads the whole audio track, so it belongs off the UI thread.
    tauri::async_runtime::spawn_blocking(move || {
        // Taken before the recording's lock and kept for the whole pass:
        // refining a boundary is a picture comparison and nothing more, so it
        // reads from the proxy where there is one.
        let pictures = with_pictures(&app, |s, _| Ok(s.clone()))?;
        let state = app.state::<Opened>();
        let guard = state.0.lock().unwrap();
        let src = guard.as_ref().ok_or("no file open")?;
        let reporter = app.clone();
        let res = detect_now(
            src,
            &pictures,
            std::sync::Arc::new(move |phase: &str, done: f64| {
                let _ = reporter.emit("cm-progress", (phase.to_string(), done));
            }),
        )?;
        // Written down whichever window asked for it: it is the recording's
        // answer, not the window's, and the list is where it will be wanted
        // next. Out from under the recording's lock first -- the editor has
        // to keep answering, and this is a file being written.
        let path = src.path.clone();
        drop(guard);
        remember_cm(&app, &path, &res);
        Ok(res)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The clip list's detection, against a recording nothing is looking at.
///
/// Opened afresh rather than through [`Opened`], because the list runs this
/// while another recording is being edited; the seek index built when the
/// clip was added is what makes that open cheap. There is no proxy for a
/// clip that was never opened, so the recording answers for its own pictures
/// when a boundary is refined.
///
/// [`BatchStop`] is read at the ends and not in the middle: none of the three
/// passes takes a stop, so asking the list to stop lands between clips rather
/// than inside one.
#[tauri::command]
async fn detect_cm_at(path: String, app: tauri::AppHandle) -> Result<CmResult, String> {
    off_thread(move || {
        let mine = app.state::<BatchStop>().cm.load(Ordering::SeqCst);
        let stopped = || app.state::<BatchStop>().cm.load(Ordering::SeqCst) != mine;
        if stopped() {
            return Err("cancelled".into());
        }
        let (src, _) = scan_cached(&app, &path)?;
        let reporter = app.clone();
        let owned = path.clone();
        let res = detect_now(
            &src,
            &src,
            std::sync::Arc::new(move |phase: &str, done: f64| {
                let _ =
                    reporter.emit("clip-cm-progress", (owned.clone(), phase.to_string(), done));
            }),
        )?;
        if stopped() {
            return Err("cancelled".into());
        }
        remember_cm(&app, &path, &res);
        Ok(res)
    })
    .await
}

/// Which of a recording's streams to leave out, from the two ways of naming
/// one.
///
/// A PID can name more than one stream. A Blu-ray's lossless sound arrives as
/// a TrueHD track with an AC-3 track folded into it, both on the one PID and
/// both handed over separately by the demuxer -- so switching that track off
/// has to switch off both halves of it, which is what asking by PID means.
fn streams_to_drop(
    src: &Source,
    by_index: Option<Vec<usize>>,
    by_pid: Option<Vec<i32>>,
) -> Vec<usize> {
    let on = src
        .audios
        .iter()
        .map(|a| (a.pid, a.stream_index))
        .chain(src.captions.iter().map(|c| (c.pid, c.stream_index)));
    resolve_pids(on, by_index.unwrap_or_default(), &by_pid.unwrap_or_default())
}

/// The indices `named` gives for the PIDs asked for, added to those already
/// asked for by index.
fn resolve_pids(
    named: impl Iterator<Item = (i32, usize)>,
    mut out: Vec<usize>,
    pids: &[i32],
) -> Vec<usize> {
    if pids.is_empty() {
        return out;
    }
    for (pid, index) in named {
        if pids.contains(&pid) && !out.contains(&index) {
            out.push(index);
        }
    }
    out
}

/// One sound track the output settings screen is answering for: what it is
/// now, and where its cut is going.
#[derive(Deserialize)]
struct TrackAsk {
    /// libav's own name for the codec, as [`AudioTrackInfo`] sent it out.
    codec: String,
    channels: u16,
    rate: u32,
    bits: u8,
    /// Whether this clip's output is a transport stream, which is the one
    /// thing about the container that decides a codec.
    ts: bool,
}

/// The answers the screen is holding for the sound. Zero, and an empty name,
/// mean the recording's own -- the `入力と同じ` at the top of every one of
/// those lists.
#[derive(Deserialize)]
struct SoundHeld {
    codec: String,
    channels: u16,
    rate: u32,
    bits: u8,
    bitrate: usize,
}

/// The lists the screen offers, and -- the same shape, since the answer to
/// "which of these may be chosen" is a list of the same kind -- which of
/// them may be.
#[derive(Deserialize, Serialize, Default)]
struct SoundList {
    codecs: Vec<String>,
    channels: Vec<u16>,
    rates: Vec<u32>,
    bits: Vec<u8>,
    bitrates: Vec<usize>,
}

/// Which of the audio settings on offer this list of clips could be written
/// with.
///
/// Not every combination can be. Blu-ray LPCM -- the only linear PCM a
/// transport stream can declare -- is written at 48, 96 and 192 kHz and
/// nowhere between; DTS is written in five channel arrangements and no
/// others, and has a bitrate floor that moves with the channels and the
/// rate. A window that offers those anyway is a window whose answer is found
/// out at the end of an export.
///
/// The engine answers rather than the window, because the answers are
/// libav's encoders' own and a table kept here would drift from whatever
/// FFmpeg the build was linked against. Off the UI thread because answering
/// means opening encoders -- a few dozen of them, none of which writes
/// anything.
#[tauri::command]
async fn audio_limits(
    tracks: Vec<TrackAsk>,
    held: SoundHeld,
    offer: SoundList,
) -> Result<SoundList, String> {
    off_thread(move || {
        let tracks: Vec<smartcut_core::SoundAsIs> = tracks
            .into_iter()
            .map(|t| smartcut_core::SoundAsIs {
                codec: t.codec,
                channels: t.channels,
                sample_rate: t.rate,
                bits: t.bits,
                to_ts: t.ts,
            })
            .collect();
        // The mode is not asked about: these five controls describe an
        // encode, the screen only shows them while one is being asked for,
        // and what an encoder will open at is the same question whether one
        // frame goes through it or all of them.
        let opts = smartcut_core::CutOptions {
            audio_mode: smartcut_core::AudioMode::Reencode,
            audio_codec: smartcut_core::AudioCodec::parse(&held.codec).unwrap_or_default(),
            audio_channels: (held.channels > 0).then_some(held.channels),
            audio_sample_rate: (held.rate > 0).then_some(held.rate),
            audio_bits: (held.bits > 0).then_some(held.bits),
            audio_bit_rate: (held.bitrate > 0).then_some(held.bitrate),
            ..Default::default()
        };
        let offered = smartcut_core::SoundChoices {
            // A name this build does not know is not on offer at all: it can
            // only have come from a newer window, and nothing here can say
            // whether it could be written.
            codecs: offer.codecs.iter().filter_map(|c| smartcut_core::AudioCodec::parse(c)).collect(),
            channels: offer.channels,
            sample_rates: offer.rates,
            bits: offer.bits,
            bit_rates: offer.bitrates,
        };
        let can = smartcut_core::writable_sound(&tracks, &opts, &offered);
        Ok(SoundList {
            codecs: can.codecs.iter().map(|c| c.as_str().to_string()).collect(),
            channels: can.channels,
            rates: can.sample_rates,
            bits: can.bits,
            bitrates: can.bit_rates,
        })
    })
    .await
}

/// Write one clip out.
///
/// `path` names the recording to cut; without it the one that is open in the
/// editor is used. Naming it is what lets the output screen work through a
/// list of clips without opening each one into the editor first -- the seek
/// index built when the clip was added makes that open cheap.
#[tauri::command]
// The arguments are the command's interface -- what the window sends is what
// the engine takes -- so there is nothing here to group into a struct that
// would not have to be taken apart again on the other side.
#[allow(clippy::too_many_arguments)]
async fn export(
    app: tauri::AppHandle,
    ranges: Vec<(f64, f64)>,
    output: String,
    path: Option<String>,
    // All switchable from the CLI; the window offers the audio settings on the
    // output settings screen and leaves the rest at the engine's defaults.
    audio_reencode: Option<bool>,
    audio_copy: Option<bool>,
    // What the sound is written as. Nothing sent, or an empty name, is the
    // recording's own codec -- the screen sends its "as it is" the way it
    // sends every other empty control.
    audio_codec: Option<String>,
    audio_channels: Option<u16>,
    audio_bitrate: Option<usize>,
    // The other two things a sample has. Zero and nothing mean the same
    // thing for both -- follow the recording -- so the screen can send its
    // "as it is" the way it sends every other empty control.
    audio_sample_rate: Option<u32>,
    audio_bits: Option<u8>,
    audio_es: Option<bool>,
    // Streams the track menu switched off, by source stream index. Nothing
    // sent means nothing dropped, which is what a clip nobody opened the
    // menu on amounts to.
    drop_streams: Option<Vec<usize>>,
    // Streams switched off in the chooser when a disc was read, by PID.
    //
    // A PID and not an index because the chooser answers before anything is
    // open: the disc's index names a track by the PID it sits on, and a
    // stream index is a thing libavformat makes up once it has read the
    // recording. Resolved here, where the recording *is* open.
    drop_pids: Option<Vec<i32>>,
) -> Result<(), String> {
    // Cutting is minutes of I/O on a broadcast recording; keeping it off the
    // UI thread is what lets the progress bar move at all.
    tauri::async_runtime::spawn_blocking(move || {
        // The output folder is typed as often as it is picked, so it too can
        // name a share: `smb://nas/rec` lands on the mount the same way an
        // input does. A plain path is handed back untouched.
        let output = local_path(&output)?.to_string_lossy().into_owned();
        // Owned either way, so the recording's lock is not held for the
        // minutes the cut takes: the editor has to keep answering while its
        // own output runs.
        let mut src = match &path {
            Some(p) => scan_cached(&app, p)?.0,
            None => app
                .state::<Opened>()
                .0
                .lock()
                .unwrap()
                .as_ref()
                .ok_or("no file open")?
                .clone(),
        };
        // Which pictures a GOP leads with decides where a copy can start.
        // The editor's plan panel has usually settled that already, but a
        // clip going straight from the list to the output screen was never
        // planned, and a fresh open knows nothing of it either way.
        if !src.leading_known {
            index::refine_leading(
                &src.path.clone(),
                &src.video.clone(),
                src.start_time,
                &mut src.points,
                &ranges,
            )
            .map_err(|e| e.to_string())?;
        }
        let plans = build_plan(&src, &ranges);
        let reporter = app.clone();
        // Tagged with the recording it belongs to: the output screen runs
        // through a list, and an untagged fraction would move whichever row
        // happened to be on screen.
        let whose = src.path.clone();
        let opts = smartcut_core::CutOptions {
            audio_mode: if audio_reencode.unwrap_or(false) {
                smartcut_core::AudioMode::Reencode
            } else if audio_copy.unwrap_or(false) {
                smartcut_core::AudioMode::Copy
            } else {
                smartcut_core::AudioMode::Smart
            },
            // Like a downmix, a codec that is not the recording's has no
            // copy path, and the engine answers one with a whole-track
            // re-encode whatever the mode above says. A name the engine does
            // not know is not something to fail a cut over: it can only come
            // from a project file written by a version that had it, and the
            // recording's own codec is the honest answer to a name that no
            // longer means anything.
            audio_codec: audio_codec
                .as_deref()
                .and_then(smartcut_core::AudioCodec::parse)
                .unwrap_or_default(),
            // A channel count that is not the recording's is a downmix, and
            // the engine answers one with a whole-track re-encode whatever
            // the mode above says. Zero and nothing mean the same thing here
            // -- follow the recording -- so the screen can send its "as it
            // is" the way it sends every other empty control.
            audio_channels: audio_channels.filter(|&c| c > 0),
            audio_bit_rate: audio_bitrate.filter(|&b| b > 0),
            // A rate or a width that is not the recording's leaves no frame
            // to copy either, and the engine answers both the same way it
            // answers a downmix: by re-encoding the whole track.
            audio_sample_rate: audio_sample_rate.filter(|&r| r > 0),
            audio_bits: audio_bits.filter(|&b| b > 0),
            drop_streams: streams_to_drop(&src, drop_streams, drop_pids),
            ..Default::default()
        };
        smartcut_core::cut_with_progress(
            &src,
            &plans,
            &output,
            &opts,
            Some(Box::new(move |f| {
                let _ = reporter.emit("export-progress", (whose.clone(), f));
            })),
        )
        .map_err(|e| e.to_string())?;

        // Read back out of the file just written, so what sits beside the
        // video is by construction the audio that is in it.
        if audio_es.unwrap_or(false) {
            let beside = std::path::Path::new(&output).with_extension("aac");
            smartcut_core::write_audio_es(
                &output,
                &beside.to_string_lossy(),
                smartcut_core::AacVersion::Auto,
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Play the edited timeline back from `from`, as a stream of pictures.
///
/// Paced against a wall clock on the *edited* timeline, so a cut costs no
/// time; pictures whose moment has already gone are dropped rather than
/// shown late, which keeps playback in time instead of letting it run slow.
#[tauri::command]
async fn play(
    app: tauri::AppHandle,
    ranges: Vec<(f64, f64)>,
    from: f64,
    width: u32,
    fps: f64,
) -> Result<(), String> {
    // The pictures come from the proxy when there is one; the sound always
    // comes from the recording, which is where the audio actually is.
    let src = with_pictures(&app, |s, _| Ok(s.clone()))?;
    let audio_from = {
        let state = app.state::<Opened>();
        let guard = state.0.lock().unwrap();
        guard.as_ref().ok_or("no file open")?.clone()
    };
    app.state::<Playing>().0.store(true, Ordering::SeqCst);

    // Audio runs on its own thread and its own clock (see `play_audio`'s
    // doc comment): it just keeps a ring buffer fed, and the sound card
    // paces itself. `Playing` is the one thing the two sides share, so
    // stopping either one stops both.
    let audio_handle = audio_from.audio.is_some().then(|| {
        let audio_src = audio_from.clone();
        let audio_ranges = ranges.clone();
        let audio_app = app.clone();
        std::thread::spawn(move || {
            let stop_app = audio_app.clone();
            let stop = move || !stop_app.state::<Playing>().0.load(Ordering::SeqCst);
            if let Err(e) = smartcut_core::play_audio(&audio_src, &audio_ranges, from, stop) {
                eprintln!("audio playback: {e}");
                // Otherwise this fails in total silence: the release build has
                // no console (`windows_subsystem = "windows"`), so eprintln
                // goes nowhere the user can see, and the video half plays on
                // regardless -- the picture looks fine, only the sound is
                // missing, with no sign of why.
                let _ = audio_app.emit("audio-error", e.to_string());
            }
        })
    });

    tauri::async_runtime::spawn_blocking(move || {
        let playing = app.state::<Playing>();
        let began = std::time::Instant::now();
        let mut elapsed_out = 0.0f64;
        let mut next_show = 0.0f64;
        let mut outcome: Result<(), String> = Ok(());

        for (a, b) in ranges {
            if !playing.0.load(Ordering::SeqCst) {
                break;
            }
            let start = a.max(from);
            if start >= b - 1e-9 {
                // wholly before the playhead: it still occupies output time
                if b > from {
                    elapsed_out += b - start.min(b);
                }
                continue;
            }
            let base = elapsed_out;
            let seg_from = start;
            let gap = 1.0 / fps.max(1.0);
            let r = smartcut_core::play_from(
                &src,
                start,
                b,
                width,
                |t| {
                    if !playing.0.load(Ordering::SeqCst) {
                        return smartcut_core::Pace::Stop;
                    }
                    let out_t = (base + t - seg_from).max(0.0);
                    let due = std::time::Duration::from_secs_f64(out_t);
                    let now = began.elapsed();
                    if due > now {
                        std::thread::sleep(due - now);
                    }
                    // Only so many pictures a second are worth sending: each
                    // one is a JPEG to encode and a data URL for the webview
                    // to take apart, and beyond a dozen or so nothing is
                    // gained by the eye. Everything between costs a decode
                    // and no more.
                    if out_t + 1e-9 < next_show {
                        return smartcut_core::Pace::Skip;
                    }
                    next_show = out_t + gap;
                    smartcut_core::Pace::Show
                },
                |t, jpeg| {
                    let _ = app.emit("play-frame", (t, as_url(&jpeg)));
                },
            );
            if let Err(e) = r {
                outcome = Err(e.to_string());
                break;
            }
            elapsed_out += b - start;
        }

        playing.0.store(false, Ordering::SeqCst);
        if let Some(h) = audio_handle {
            let _ = h.join();
        }
        let _ = app.emit("play-ended", ());
        outcome
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn stop_play(playing: State<Playing>) {
    playing.0.store(false, std::sync::atomic::Ordering::SeqCst);
}

/// Write the keyframe list beside the output.
///
/// One frame number per line, CRLF, and nothing else -- the shape the tools
/// that read these files expect. Numbered against the file just written, not
/// the recording it came from.
#[tauri::command]
fn write_keyframes(path: String, frames: Vec<u32>, fps: f64) -> Result<usize, String> {
    use std::fmt::Write as _;
    let _ = fps;
    let mut body = String::new();
    for f in &frames {
        let _ = write!(body, "{f}\r\n");
    }
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    Ok(frames.len())
}

/// Read a keyframe list back, for the sidecar beside a recording being opened.
///
/// `Ok(None)` is "there is no such file", which is the ordinary case and not
/// an error: most recordings have no marks saved next to them.
///
/// The write side emits bare numbers, but files that arrive from elsewhere
/// carry a `# keyframe format v1` header and an `fps` line above them. Lines
/// that are not a frame number are skipped rather than refused: a mark list
/// is an aid, and failing to open the recording over a line nobody reads
/// would be the worse trade.
#[tauri::command]
fn read_keyframes(path: String) -> Result<Option<Vec<u32>>, String> {
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    Ok(Some(
        body.lines().filter_map(|l| l.trim().parse::<u32>().ok()).collect(),
    ))
}

/// Write a project file.
///
/// The list window builds the text: a project is the rows, what has been cut
/// out of each of them and where the results are to go, and all of that is up
/// there. Same division as [`write_keyframes`] -- whoever holds the state
/// owns the shape of the file, and this side owns the disc.
#[tauri::command]
fn write_project(path: String, body: String) -> Result<(), String> {
    std::fs::write(&path, body)
        .map_err(|e| trf!("保存できません: {} ({})", "Cannot save: {} ({})", path, e))
}

/// Read one back.
///
/// A missing file is an error here, unlike the keyframe sidecar's: that one
/// is looked for on the off-chance, and this one was named by the user out of
/// a file picker and is expected to be there.
#[tauri::command]
fn read_project(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path)
        .map_err(|e| trf!("開けません: {} ({})", "Cannot open: {} ({})", path, e))
}

/// Whether the list is holding work that is not on disc, as the frontend
/// last said.
///
/// Worked out up there, where the work is; kept down here for the one thing
/// that cannot be done from a page -- refusing to let the window close on it.
static DIRTY: AtomicBool = AtomicBool::new(false);

/// Told whenever the answer changes, the same way the language is: the
/// frontend is the side that knows, and this side is only ever informed.
#[tauri::command]
fn set_dirty(dirty: bool) {
    DIRTY.store(dirty, Ordering::Relaxed);
}

/// Close for good, once the question about unsaved work has been answered.
///
/// The flag goes down first, or the close this asks for would be stopped by
/// the very check that asked the question.
#[tauri::command]
fn quit(app: tauri::AppHandle) {
    DIRTY.store(false, Ordering::Relaxed);
    app.exit(0);
}

/// The language this side should write in, as the frontend settled it.
///
/// Said once at startup and again whenever it is changed in 環境設定. The
/// frontend holds the preference -- it is the one with somewhere to keep it
/// and the one that knows what "follow the machine" came to -- so this is
/// only ever told, never asked.
#[tauri::command]
fn set_lang(lang: String) {
    lang::set(&lang);
}

/// What the machine is set to, for the frontend to check its own answer
/// against. Nothing on the platforms where the webview knows better.
#[tauri::command]
fn os_locale() -> Option<String> {
    lang::from_os()
}

/// What the バージョン情報 panel prints.
///
/// Asked for rather than written into the frontend, because two of these
/// three are only knowable from here: the version is the one the binary was
/// stamped with at build time, and the libav numbers are the ones of the
/// libraries this process actually loaded. A version the About box holds a
/// copy of is a version that goes stale the first time a release forgets to
/// update it, and the report it came back in is then wrong about the build
/// it is reporting.
#[derive(Serialize)]
struct Versions {
    /// The application's, from the manifest the window was built from.
    app: String,
    /// The cutting engine's. The same number today -- one workspace, one
    /// version -- and named separately anyway, because the engine is also
    /// the CLI's and need not stay in step forever.
    core: String,
    avformat: String,
    avcodec: String,
    avutil: String,
    /// libav's licence, which is not this program's.
    ffmpeg_license: String,
    /// The machine this build is for, as a bug report would have to say it.
    platform: String,
}

#[tauri::command]
fn versions() -> Versions {
    let av = smartcut_core::libav();
    Versions {
        app: env!("CARGO_PKG_VERSION").to_string(),
        core: smartcut_core::VERSION.to_string(),
        avformat: av.avformat,
        avcodec: av.avcodec,
        avutil: av.avutil,
        ffmpeg_license: av.license,
        platform: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WebKitGTK's compositor draws nothing on a machine without a GPU: the
    // window paints once and then never updates, which looks exactly like a
    // frozen app. This UI has no need of it.
    if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    }
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    // The same freeze from the other end, and this one waits until a text
    // field is clicked: with GTK's XIM input-method module in the window,
    // WebKitGTK stops painting the moment an `<input>` takes focus. The page
    // is alive underneath -- scripts run, clicks land, the screen behind the
    // stale pixels goes on changing -- and resizing the window brings it all
    // back at once, which is what says it is the painting and not the
    // program. The output settings screen is where it was found, because its
    // filename prefix is the first field in the program you can click into.
    //
    // XIM is not chosen here; it is what GTK falls back to. With
    // `GTK_IM_MODULE` unset -- which is every desktop where an IME was never
    // set up -- GTK picks a module by locale, and `im-xim.so` claims
    // ja:ko:th:zh. So a Japanese desktop with no IME configured is exactly
    // the machine this lands on, which is most of the ones this is for.
    //
    // `gtk-im-context-simple` is the answer rather than a real IME because it
    // is the one module built into GTK: naming a module that is not installed
    // does not fall back to it, it falls back to XIM, which is the freeze
    // again. It cannot compose Japanese, and on these machines nothing could
    // -- there was no IME to lose. Anyone who has one has `GTK_IM_MODULE` set
    // already, and an explicit choice is left alone.
    let im = std::env::var("GTK_IM_MODULE").unwrap_or_default();
    if im.is_empty() || im == "xim" {
        std::env::set_var("GTK_IM_MODULE", "gtk-im-context-simple");
    }

    // Whatever the desktop is set to, until the frontend says otherwise.
    // Anything said before then -- a file named on the command line that
    // cannot be opened, most of all -- is already in the right language.
    if let Some(tag) = lang::from_os() {
        lang::set(&tag);
    }

    let argv = Argv(std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect());
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Opened::default())
        .manage(Proxy::default())
        .manage(Generation::default())
        .manage(Thumbs::default())
        .manage(OpenPath::default())
        .manage(Held::default())
        .manage(Playing::default())
        .manage(BatchStop::default())
        .manage(argv)
        // The list window is declared in the configuration rather than built
        // here, so `setup` is the first moment there is one to attach
        // anything to.
        .setup(|app| {
            if let Some(w) = app.get_webview_window(MAIN) {
                let asker = app.handle().clone();
                w.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        // Held open while the question is put. The frontend
                        // answers by calling `quit`, or by doing nothing --
                        // there is no third thing to wait for, so nothing is
                        // remembered about having asked.
                        if DIRTY.load(Ordering::Relaxed) {
                            api.prevent_close();
                            let _ = asker.emit("close-requested", ());
                        }
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            log,
            initial_paths,
            resolve_paths,
            open_source,
            detect_cm,
            thumbs_at,
            preview,
            prepare,
            hover_thumb,
            scene_search,
            make_plan,
            write_keyframes,
            read_keyframes,
            play,
            stop_play,
            export,
            audio_limits,
            index_clip,
            detect_cm_at,
            cm_cached,
            stop_batch,
            clip_plan,
            tracks,
            clip_thumbs,
            clip_poster,
            clip_glance,
            open_editor,
            retitle_editor,
            retitle_main,
            close_editor,
            write_project,
            read_project,
            set_dirty,
            quit,
            set_lang,
            os_locale,
            versions
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Blu-ray's sound, as the demuxer hands it over: the lossless track
    /// and the AC-3 it is wrapped around arrive as two streams on one PID.
    /// Switching that track off has to switch off both halves of it, or the
    /// cut carries the sound the chooser was told to leave out.
    #[test]
    fn a_track_switched_off_by_pid_takes_both_halves_of_itself() {
        let streams = [(0x1100, 1), (0x1100, 2), (0x1101, 3), (0x1101, 4)];
        assert_eq!(
            resolve_pids(streams.into_iter(), Vec::new(), &[0x1100]),
            vec![1, 2]
        );
        // Both ways of naming a stream, and neither said twice.
        assert_eq!(
            resolve_pids(streams.into_iter(), vec![2], &[0x1100]),
            vec![2, 1]
        );
        // A PID the recording does not carry drops nothing, and nothing asked
        // for leaves what the editor asked for alone.
        let none: Vec<usize> = Vec::new();
        assert_eq!(resolve_pids(streams.into_iter(), Vec::new(), &[0x1200]), none);
        assert_eq!(resolve_pids(streams.into_iter(), vec![3], &[]), vec![3]);
    }
}
