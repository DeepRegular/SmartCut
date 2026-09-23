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
//! Those two halves run **at the same time**: an index pass, a detection and
//! an open cut editor, all at once. Sharing nothing is what makes that safe;
//! what makes it bearable is that the background passes hold themselves to
//! part of the machine while the editor is up (see [`background_threads`]),
//! because the picture under the pointer is the one somebody is waiting for.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

#[macro_use]
mod lang;
mod geometry;
mod prefs;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use smartcut_core::{index, netpath, plan_on, proxy, seek_index, PlanOptions, SeekIndex, Source};
use tauri::{Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

#[derive(Default)]
struct Opened(Mutex<Option<Source>>);

/// A lock a panic cannot take away for good.
///
/// `Mutex::lock` refuses ever after once a thread has panicked while holding
/// it, and `unwrap` on that refusal turns one unreadable recording into a
/// window that answers nothing until it is restarted -- the film strip, the
/// preview and the list all read through these. Everything behind them is a
/// cache of what is open, so what a panic left there is worth no less than
/// the lock: the poison is stepped over and the next open writes over it.
fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

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

/// Which run of playback is the one in force, or 0 for none.
///
/// A number rather than a flag, because a stop and the next start are not
/// always far apart: ループ pressed while something is playing stops that
/// playback and asks for another -- over the selection this time -- inside
/// one turn of the window's own loop, and a hand on 停止 and 再生 does the
/// same a little slower. Told apart by a flag, the run that was asked to
/// stop can still be between two pictures when the new one sets the flag
/// back to true -- and it then reads that as permission to carry on, so two
/// playbacks run at once and the one being stopped is the one further along,
/// whose pictures win. The round that was asked for never appeared to start.
///
/// The window names each run; a run carries on only while the name in here
/// is still its own. See `startPlay` on the other side.
#[derive(Default)]
struct Playing(std::sync::atomic::AtomicU64);

/// How loud the preview plays, held here rather than in the window.
///
/// The level has to outlive one playback: it is set once and then holds for
/// every 再生 after it, and the thread that plays is started fresh each
/// time. It also has to be reachable *during* one -- the slider is moved
/// while the sound is running -- and [`smartcut_core::Volume`] is the shared
/// number both sides of that hold. See [`set_volume`].
#[derive(Default)]
struct Vol(smartcut_core::Volume);

/// How loud each channel has been since the meter last drew.
///
/// Written by the audio output callback and emptied by the window that reads
/// it, which is why it lives out here rather than in either of them. See
/// [`smartcut_core::Levels`] and [`audio_levels`].
#[derive(Default)]
struct Meter(smartcut_core::Levels);

/// Whether the cut editor is on screen.
///
/// Playback belongs to that window, and [`Playing`] on its own cannot say so.
/// The window is closed on the thread it is drawn on while [`play`] switches
/// playback on from a worker, so a 再生 the machine is too busy to act on at
/// once -- which is what 解析中 means -- can land *after* the close, set
/// `Playing` back to true, and start an audio thread with nothing left alive
/// to stop it: the recording then plays on to its end out of a window that is
/// not there. Both halves of playback therefore ask this as well, and a
/// `play` that arrives after its window has gone does not start at all.
#[derive(Default)]
struct EditorUp(std::sync::atomic::AtomicBool);

/// Whether the seam window is on screen, and the two clips it is looking at.
///
/// The same pair of reasons [`EditorUp`] exists, for the same window-shaped
/// hole: playback started from a worker can land after the window that asked
/// for it has gone, and a run with nothing left to stop it plays the rest of
/// the recording out of a window that is not there.
///
/// The clips are held beside the flag rather than in [`Opened`] because they
/// are not what is open: the seam window is reached from the clip list, which
/// may have a recording open in the cut editor at the same time, and a seam
/// is two recordings where [`Opened`] is one.
#[derive(Default)]
struct CrossUp(std::sync::atomic::AtomicBool);

#[derive(Default)]
struct Crossed(Mutex<Option<CrossPair>>);

struct CrossPair {
    before_path: String,
    after_path: String,
    before: Source,
    after: Source,
}

/// The subtitle track the preview is drawing, once one has been asked for.
///
/// Kept between frames because that is the whole point of it: the reader
/// holds the recording open and a stretch of the subtitles decoded, so
/// scrubbing inside that stretch costs a lookup. Built on the first frame
/// the window asks about, thrown away when another track is asked for or
/// another recording is opened. See [`smartcut_core::subs`].
#[derive(Default)]
struct Subs(Mutex<Option<Subtitles>>);

struct Subtitles {
    /// Which track, by the number the recording names it with -- a PID, or
    /// a DVD's substream id.
    id: i32,
    reader: smartcut_core::subs::Reader,
}

/// How many times each of the clip list's background lanes has been asked to
/// give up.
///
/// One count per lane, because stopping one of them may not touch the others:
/// opening the cut editor on a clip stops whichever lane is on *that* clip
/// and leaves the other two working. The walk and the pictures are counted
/// apart for that reason -- they run at the same time now, on different
/// recordings, and a stop meant for one of them would otherwise throw away
/// the other's minutes of work.
///
/// A count rather than a flag. A pass takes the number as it starts and
/// gives up as soon as it sees a larger one, so a stop lands on exactly the
/// passes that were running when it was asked for. A flag would have to be
/// lowered again before the next pass could start, and there is no moment to
/// lower it in: the other lanes are still watching it.
#[derive(Default)]
struct BatchStop {
    walk: AtomicU64,
    pics: AtomicU64,
    cm: AtomicU64,
    /// The two flat detections, counted apart because they are asked for
    /// apart: a list told to find the silences and nothing else must not be
    /// stopped by a row being taken out from under the pictures pass.
    blank: AtomicU64,
    quiet: AtomicU64,
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

/// Takes the tracks rather than the recording, because the cheap first look
/// at a file has them before there is a [`Source`] to hold them. See
/// [`clip_outline`].
fn audio_tracks_of(audios: &[smartcut_core::AudioInfo]) -> Vec<AudioTrackInfo> {
    audios
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

/// One subtitle track, for the preview's own picker.
///
/// Carried with the rest of what a recording is, because it is free here:
/// the same open that found the sound found these. See
/// [`smartcut_core::subs`], which is what draws them.
#[derive(Serialize)]
struct SubtitleTrackInfo {
    /// A PID in a transport stream, a substream id on a DVD -- the number
    /// [`subtitle_at`] is asked for.
    id: i32,
    /// "caption" for a broadcast's ARIB captions, "graphics" for a
    /// Blu-ray's, "subpicture" for a DVD's.
    kind: String,
    language: Option<String>,
}

fn subtitle_tracks_of(
    captions: &[smartcut_core::CaptionInfo],
    graphics: &[smartcut_core::GraphicsInfo],
    subpictures: &[smartcut_core::SubpictureInfo],
) -> Vec<SubtitleTrackInfo> {
    smartcut_core::subs::tracks(captions, graphics, subpictures)
        .into_iter()
        .map(|t| SubtitleTrackInfo {
            id: t.id,
            kind: match t.kind {
                smartcut_core::subs::Kind::Caption => "caption",
                smartcut_core::subs::Kind::Superimpose => "superimpose",
                smartcut_core::subs::Kind::Ttml => "ttml",
                smartcut_core::subs::Kind::Graphics => "graphics",
                smartcut_core::subs::Kind::Subpicture => "subpicture",
            }
            .into(),
            language: t.language,
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
    /// Whether the pictures come at a rate a frame number could be counted
    /// off. See [`smartcut_core::VideoInfo::variable_rate`].
    variable: bool,
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
    /// Every subtitle track, so the editor can offer to draw one over the
    /// preview. See [`SubtitleTrackInfo`].
    subtitles: Vec<SubtitleTrackInfo>,
    index_name: String,
    /// Presentation times of every random access point: the places a cut
    /// costs nothing.
    points: Vec<f64>,
    /// How many of those cannot start a copy because their leading pictures
    /// are referenced.
    unusable_points: usize,
    /// Where the material begins, for a window that has `points` empty: the
    /// first picture a cut could start from, read off the front of the file
    /// rather than found by the walk. `points[0]` says it once the walk has
    /// landed and this says it before then, so the timeline, the frame
    /// counter and a mark file's numbers are right from the moment the window
    /// opens. See `smartcut_core::first_picture` and `headTime` in `main.js`.
    head: Option<f64>,
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
    /// Whether the pictures come at a rate a frame number could be counted
    /// off. See [`smartcut_core::VideoInfo::variable_rate`].
    variable: bool,
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
    /// The pictures, where this read produced them. Present only on the
    /// one-read path -- a recording on a share, where reading it twice would
    /// be transferring it twice -- and `None` where the list is to ask for
    /// them separately through [`clip_pictures`]. See [`one_read`].
    pictures: Option<ClipPictures>,
    /// Whether the seek index was already on disc from an earlier session,
    /// in which case this cost a read and not a pass over the recording.
    cached: bool,
    seconds: f64,
    /// Bits a second the pictures take, and bits a second the sound takes.
    ///
    /// What the disc gauge on the output settings screen is drawn from. They
    /// travel with the row because neither of them changes when a range does:
    /// the gauge is redrawn every time somebody moves a cut, and opening the
    /// recording again to ask would be reading a file to find out something
    /// already known.
    video_rate: f64,
    audio_rate: f64,
    /// Whether the pictures of this recording can be written back smaller --
    /// which is to say whether they are MPEG-2. See
    /// `smartcut_core::cut::CutOptions::video_share`.
    can_shrink: bool,
}

/// What the container itself says about a recording, had in tens of
/// milliseconds however long the file is.
///
/// Every field here comes out of libavformat's probe -- a few megabytes off
/// the head of the file -- and not out of the walk, which is a pass over
/// every byte at around a second a gigabyte. So it is what a row can be given
/// the instant a file is dropped on the list, instead of a path and a blank
/// rectangle for as long as the queue in front of it takes.
///
/// What is *not* here is everything the walk answers for: where the lossless
/// points are, how many of them cannot start a cut, and whether the stream
/// carries pulldown. A row says those are not known yet until [`index_clip`]
/// has been over it. The length is here but is the container's own, which on
/// a program stream can be wildly wrong -- see [`smartcut_core::Outline`].
#[derive(Serialize)]
struct ClipOutline {
    path: String,
    name: String,
    codec: String,
    width: u32,
    height: u32,
    fps: f64,
    duration: f64,
    frames: u64,
    interlaced: bool,
    has_audio: bool,
    audio_channels: u16,
    audio_sample_rate: u32,
    audio_bits: u8,
    audio_tracks: Vec<AudioTrackInfo>,
}

/// What the picture pass leaves behind: the row's own picture and the scenes.
#[derive(Serialize)]
struct ClipPictures {
    /// A picture from a little way in, for the row to show. Absent only when
    /// the pass produced no pictures at all.
    poster: Option<String>,
    scenes: usize,
    /// Whether the pictures were already on disc, in which case this cost a
    /// read of one file and no decoding at all.
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
        /// Whether the disc's streams are encrypted, which is a thing to say
        /// before somebody ticks rows that will not open.
        protected: bool,
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
            protected: disc.protected,
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
    /// What the disc's own playlist said about the recording: when it was
    /// made, what it was about, and which channel it came off.
    ///
    /// Travels with the row for the same reason the label does: a cut of
    /// this recording written onto a disc of its own says the programme was
    /// what this disc says it was.
    made: Option<String>,
    /// How long the programme ran on air, in seconds.
    ran: Option<u32>,
    description: Option<String>,
    channel: Option<String>,
    channel_number: u16,
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
            made: e.made.map(|m| m.to_string()),
            ran: e.ran,
            description: e.description,
            channel: e.channel,
            channel_number: e.channel_number,
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
/// Off the window thread, like everything else that touches a path somebody
/// chose.
///
/// What this does to each input is a `stat`, and for a folder a walk of it,
/// and for a disc a read of its index -- and the paths are whatever was
/// dropped on the window, which for this program is as often a share as a
/// local disc. Done where a `#[tauri::command]` that is not `async` is done,
/// which is the thread the window is drawn on, dropping a folder froze the
/// window until the folder had been read, and dropping one on a share that
/// had gone to sleep froze it until the share woke up.
#[tauri::command]
async fn resolve_paths(paths: Vec<String>) -> Vec<Resolved> {
    let asked = paths.clone();
    tauri::async_runtime::spawn_blocking(move || {
        paths
            .into_iter()
            .map(|input| match files_at(&input) {
                Ok(files) => Resolved { input, files, error: None },
                Err(error) => Resolved { input, files: Vec::new(), error: Some(error) },
            })
            .collect()
    })
    .await
    // A pass that ended badly answers for every input it was given rather
    // than for none: a list that came back empty would look like a drop of
    // nothing at all.
    .unwrap_or_else(|e| {
        asked
            .into_iter()
            .map(|input| Resolved { input, files: Vec::new(), error: Some(e.to_string()) })
            .collect()
    })
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
        let guard = locked(&state.0);
        if let Some(p) = guard.as_ref() {
            return f(&p.src, Some(&p.marks));
        }
    }
    let state = app.state::<Opened>();
    let guard = locked(&state.0);
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

/// As [`off_thread`], for a pass that is to stand behind the rest of the
/// machine: reading a recording, building a proxy, detecting commercials.
///
/// A thread of its own rather than the pooled one [`off_thread`] borrows.
/// The priority a pass takes on cannot be given back -- see
/// [`smartcut_core::nice`] -- so it has to be taken by a thread that exists
/// for the pass and ends with it. A thread out of the pool would carry it
/// into whatever it was handed next, and what it is handed next may well be
/// the export. The pooled thread only waits here for the answer, which costs
/// nothing: it is asleep either way.
async fn off_thread_behind<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    off_thread(move || {
        let hand = std::thread::spawn(move || {
            smartcut_core::nice::behind();
            f()
        });
        // A pass that panicked is reported as an error rather than carried
        // back out as a panic. It is what the pooled thread would have done
        // with it, said in the shape every other failure here is said in.
        hand.join().map_err(|_| "pass ended badly".to_string())?
    })
    .await
}

#[tauri::command]
async fn open_source(path: String, app: tauri::AppHandle) -> Result<SourceInfo, String> {
    off_thread(move || open_now(&path, &app)).await
}

/// What the container says about a recording, for a window that has not read
/// it yet -- the same answer [`clip_outline`] gives the list, in the shape
/// the editor speaks.
///
/// `points` is empty, and that is the whole of what is missing. Only the walk
/// finds the access points, and the walk reads every byte of the recording:
/// tens of seconds over a share, during which the editor used to show its own
/// name and nothing else. What it can show without them is everything except
/// where a cut is free -- the length, the picture, the timeline, the marks --
/// so it shows that, and the points arrive behind it. See `openPath`.
///
/// Nothing is stored: there is no [`Source`] here to store, and every command
/// that wants one still has to wait for [`open_source`]. This answers a
/// question, it does not open anything.
#[tauri::command]
async fn open_outline(path: String) -> Result<SourceInfo, String> {
    off_thread(move || {
        // The one caller that wants the head: see `Outline::head` and the
        // marks the editor puts down before the walk lands.
        let o = smartcut_core::outline_with_head(&path).map_err(|e| e.to_string())?;
        Ok(SourceInfo {
            path: o.path.clone(),
            codec: o.video.codec.clone(),
            width: o.video.width,
            height: o.video.height,
            fps: o.video.frame_rate,
            duration: o.duration,
            interlaced: o.video.interlaced(),
            // Only the walk sees either of these -- one is a flag on the
            // pictures and not in the container, the other is what the
            // pictures do rather than what the container says they do -- so
            // these say nothing rather than saying the recording is free of
            // them.
            pulldown: false,
            variable: false,
            has_audio: o.audio.is_some(),
            audio_channels: o.audio.as_ref().map_or(0, |a| a.channels),
            audio_sample_rate: o.audio.as_ref().map_or(0, |a| a.sample_rate),
            audio_bits: o.audio.as_ref().map_or(0, |a| a.bits),
            audio_tracks: audio_tracks_of(&o.audios),
            subtitles: subtitle_tracks_of(&o.captions, &o.graphics, &o.subpictures),
            index_name: String::new(),
            points: Vec::new(),
            unusable_points: 0,
            head: o.head,
            start_time: o.start_time,
        })
    })
    .await
}

/// A picture from around `time`, out of a recording nothing has been read of.
///
/// The stage's picture while the walk is still running. Approximate on
/// purpose: with no access points there is nothing to seek by but the
/// container's own guess, which on a transport stream lands near the instant
/// rather than on it. The instant that comes back is the picture's own, so
/// the frame counter follows the picture rather than the pointer.
///
/// Costs one open and one GOP. [`preview`] is the exact answer and needs the
/// walk to have finished.
#[tauri::command]
async fn glimpse(path: String, time: f64, width: u32) -> Result<Shot, String> {
    off_thread(move || {
        let s = smartcut_core::glance_at(&path, time, width).map_err(|e| e.to_string())?;
        Ok(Shot { url: as_url(&s.jpeg), time: s.time, kind: s.kind.to_string() })
    })
    .await
}

/// Pictures for the film strip, out of a recording nothing has been read of.
///
/// The strip's answer while the walk is still running, and approximate for
/// the same reason [`glimpse`] is: with no access points there is nothing to
/// seek by but the container's own guess. Each shot carries the instant it
/// actually landed on, which is the whole of what makes it usable -- the
/// window puts a picture under the cell it landed in rather than under the
/// cell that asked for it, and captions that cell with the instant it came
/// back with. A time nothing could be decoded near comes back empty rather
/// than shifting the rest along.
///
/// One open for the lot, rather than the open apiece [`glimpse`] would cost.
/// The walk is reading the same recording while this runs, and over a share
/// that is the difference between a strip that fills and a walk that stalls.
#[tauri::command]
async fn glimpses(
    path: String,
    times: Vec<f64>,
    width: u32,
    app: tauri::AppHandle,
) -> Result<Vec<Option<Shot>>, String> {
    if times.is_empty() {
        return Ok(Vec::new());
    }
    off_thread(move || {
        let shots = smartcut_core::glance_run(&path, &times, width).map_err(|e| e.to_string())?;
        Ok(shots
            .into_iter()
            .map(|o| {
                o.map(|s| Shot {
                    url: shot_url(&app, &s.jpeg),
                    time: s.time,
                    kind: s.kind.to_string(),
                })
            })
            .collect())
    })
    .await
}

/// Every entry picture in a stretch of a recording nothing has been read of.
///
/// The other way of filling the strip before the walk, for a reel drawn so
/// close in that the whole of it is a few seconds of the recording. [`glimpses`]
/// answers each cell with the entry point at or before it, which leaves a gap
/// wherever two cells fall between the same pair of entry points; this reads
/// the stretch through and finds every one of them, so a cell is empty only
/// where the recording genuinely has nothing to put in it.
///
/// Dear in proportion to the stretch -- it decodes what lies between the entry
/// points and throws it away -- so the window asks for this only while the
/// reel is short, and only where it has seen it fill cells the seeks could
/// not. See `fillByGlance`.
#[tauri::command]
async fn glimpse_sweep(
    path: String,
    from: f64,
    to: f64,
    width: u32,
    // Named as the window names it: an argument is looked up in the call by
    // this very ident, so a synonym here is a call that never arrives.
    cell: f64,
    most: usize,
) -> Result<Vec<Shot>, String> {
    off_thread(move || {
        let shots = smartcut_core::glance_sweep(&path, from, to, width, cell, most)
            .map_err(|e| e.to_string())?;
        Ok(shots
            .into_iter()
            .map(|s| Shot { url: as_url(&s.jpeg), time: s.time, kind: s.kind.to_string() })
            .collect())
    })
    .await
}

/// Where this program may put what it can always build again.
///
/// The platform's own answer, unless 環境設定 names somewhere else: a seek
/// index is a few hundred kilobytes but a proxy is gigabytes an hour, and a
/// machine whose home directory is on the small fast disk has every reason
/// to send them to the big slow one. Under it are the three folders below,
/// one per kind, so that a folder somebody chose stays legible and so that
/// one kind can be deleted without the others.
///
/// A folder chosen here is made when it is chosen, not when it is used --
/// `set_prefs` is where a path that cannot be written to is refused, which
/// is while somebody is looking at the panel that asked for it.
fn cache_root(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    if let Some(dir) = prefs::cache_dir() {
        return Ok(dir);
    }
    app.path().app_cache_dir().map_err(|e| {
        format!("{}: {e}", tr!("キャッシュの置き場が分かりません", "No cache directory"))
    })
}

/// One of the three, made if it is not there yet.
fn cache_kind(app: &tauri::AppHandle, kind: &str) -> Result<std::path::PathBuf, String> {
    let dir = cache_root(app)?.join(kind);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Where seek indexes are kept.
fn index_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    cache_kind(app, "index")
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
    scan_cached_reporting(app, path, None)
}

/// As [`scan_cached`], with somewhere to say how far through the walk is.
///
/// Said only by the walk, and the walk only happens where there is no index
/// to pick up -- so a row that finds one goes straight past this and the bar
/// never shows the phase at all, which is the honest picture of what
/// happened.
fn scan_cached_reporting(
    app: &tauri::AppHandle,
    path: &str,
    on: Option<smartcut_core::index::OnProgress>,
) -> Result<(Source, Option<SeekIndex>), String> {
    // The pass over the packets is the same answer every time, so a previous
    // session's is taken where there is one: about a second per gigabyte
    // saved, which on a half-hour recording is the whole of the wait between
    // choosing a file and being able to move the pointer.
    let mut held = held_index(app, path);
    let mut src = match &held {
        Some(ix) => smartcut_core::scan_reporting(path, ix, on),
        // Ask whoever already knows. A recording on a disc has an index
        // beside it -- the disc wrote one -- and reading it is the difference
        // between opening a UHD title in under a second and reading eighty
        // gigabytes to arrive at the same list; an MP4 or a Matroska file
        // carries a seek table of its own. The walk is what answers for a
        // transport stream, which has neither.
        None => smartcut_core::scan_reporting(path, &smartcut_core::index::DiscIndex, on)
            .or_else(|_| smartcut_core::scan_reporting(path, &smartcut_core::ContainerIndex, on))
            .or_else(|_| smartcut_core::scan_reporting(path, &smartcut_core::PacketScan, on)),
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
        src = smartcut_core::scan_reporting(path, &smartcut_core::PacketScan, on);
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
        variable: src.video.variable_rate,
        has_audio: src.audio.is_some(),
        audio_channels: src.audio.as_ref().map_or(0, |a| a.channels),
        audio_sample_rate: src.audio.as_ref().map_or(0, |a| a.sample_rate),
        audio_bits: src.audio.as_ref().map_or(0, |a| a.bits),
        audio_tracks: audio_tracks_of(&src.audios),
        subtitles: subtitle_tracks_of(&src.captions, &src.graphics, &src.subpictures),
        index_name: src.index_name.to_string(),
        points: src.points.iter().map(|p| p.time).collect(),
        unusable_points: src.points.iter().filter(|p| p.open_gop() && !p.droppable).count(),
        // The walk has been over it, so `points` is the answer and this is
        // not asked for.
        head: None,
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
    *locked(&app.state::<Thumbs>().0) = None;
    // Named by what was asked for rather than by what came back: this is
    // compared against a path the list window holds, and that is the string
    // it handed over.
    *locked(&app.state::<OpenPath>().0) = Some(path.to_string());
    *locked(&app.state::<Proxy>().0) = None;
    *locked(&app.state::<Subs>().0) = None;
    *locked(&app.state::<Held>().0) = held;
    let info = info_of(&src);
    *locked(&app.state::<Opened>().0) = Some(src);
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

/// Pictures the editor is looking at, kept where the window can fetch them
/// rather than written into the answer.
///
/// A picture written into an answer has to be base64 first, because an answer
/// is JSON. A strip refresh is thirty cells: at 200 px that is 0.21 MB of
/// JPEG written out as 0.28 MB of text, inside a 0.37 MB string for the
/// window to parse, up to four times a second while somebody drags. Handed
/// over as an address instead, the bytes travel the road an ordinary image
/// travels -- no encoding at either end, and the window's own cache keeps
/// the ones it has already been given, which matters because a strip asks
/// for the same cells over and over.
///
/// Bounded, because nothing here is ever given back: the oldest go when the
/// room runs out. The cap is far above what the editor holds at once -- a
/// reel is forty cells and the cards fifty, and the stage one picture -- so a
/// picture on screen is never one that has been dropped.
#[derive(Default)]
struct Shots(std::sync::Mutex<ShotStore>);

#[derive(Default)]
struct ShotStore {
    /// By what is in it, not by what number it was given. The strip asks for
    /// the same cells over and over -- a drag is the same reel shifted a cell
    /// at a time -- and a picture held once is the same bytes every time it
    /// is asked for, so the same picture gets the same address. The window's
    /// own cache then answers the second ask without asking us at all, and
    /// nothing is stored twice.
    held: std::collections::HashMap<u64, Kept>,
    /// When each was last asked for, oldest first, so the room is made at the
    /// right end. Marks in the editor are asked for once and kept on screen
    /// for the length of a session; a reel that churned them out would empty
    /// the cards while somebody dragged.
    order: std::collections::BTreeMap<u64, u64>,
    used: u64,
    bytes: usize,
}

/// One picture, and when it was last asked for.
struct Kept {
    jpeg: Vec<u8>,
    used: u64,
}

/// What the pictures held for the windows may come to.
///
/// A hundred times what is on screen at once and then some: a reel of cells
/// is a third of a megabyte and a stage picture a tenth of one. The window
/// keeps its own copy of whatever it has fetched, so this is the second of
/// two caches and does not need to be the larger.
const SHOT_ROOM: usize = 32 << 20;

/// What a picture is called: what is in it, in sixty-four bits.
///
/// FNV-1a over the whole JPEG. Seven kilobytes to walk and nothing to choose
/// between this and anything cleverer at that size, and the property wanted
/// is only that two different pictures are not called the same thing.
fn shot_name(jpeg: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in jpeg {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// Keep `jpeg` and hand back the address the window fetches it by.
///
/// The address is what is in the picture, so what is behind one never changes
/// and the window is told it may keep it for ever.
fn shot_url(app: &tauri::AppHandle, jpeg: &[u8]) -> String {
    let name = shot_name(jpeg);
    let state = app.state::<Shots>();
    let mut store = locked(&state.0);
    store.used += 1;
    let now = store.used;
    match store.held.get_mut(&name) {
        // Already here. It moves to the young end rather than being written
        // again: the bytes are the same bytes.
        Some(held) => {
            let was = held.used;
            held.used = now;
            store.order.remove(&was);
            store.order.insert(now, name);
        }
        None => {
            store.bytes += jpeg.len();
            store.held.insert(name, Kept { jpeg: jpeg.to_vec(), used: now });
            store.order.insert(now, name);
            while store.bytes > SHOT_ROOM {
                let Some((&oldest, &gone)) = store.order.iter().next() else {
                    break;
                };
                store.order.remove(&oldest);
                if let Some(held) = store.held.remove(&gone) {
                    store.bytes -= held.jpeg.len();
                }
            }
        }
    }
    // What the frontend's own `convertFileSrc` does with a path: a scheme of
    // its own everywhere but Windows, where a custom scheme is served under
    // `http` on a host of its own.
    if cfg!(windows) {
        format!("http://{SHOT_SCHEME}.localhost/{name:016x}")
    } else {
        format!("{SHOT_SCHEME}://localhost/{name:016x}")
    }
}

const SHOT_SCHEME: &str = "shot";

#[tauri::command]
async fn preview(time: f64, width: u32, app: tauri::AppHandle) -> Result<Shot, String> {
    off_thread(move || {
        with_pictures(&app, |src, marks| {
            let s = smartcut_core::shot_at(src, time, width).map_err(|e| e.to_string())?;
            Ok(Shot { url: shot_url(&app, &s.jpeg), time: s.time, kind: kind_of(&s, src, marks) })
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

/// How far it is from `at` to the nearest access point on either side of it,
/// not counting one standing on `at` itself. Infinite where there is none.
///
/// The ground a film strip cell has to itself. A held picture further off
/// than half of it is nearer to the neighbouring entry point than to the one
/// it is meant to answer for, and a disc's entry points are close enough
/// together for that to happen.
fn to_neighbour(points: &[smartcut_core::AccessPoint], at: f64) -> f64 {
    let i = points.partition_point(|p| p.time < at - 1e-6);
    let after = points[i..].iter().find(|p| p.time > at + 1e-6).map(|p| p.time - at);
    let before = points[..i].last().map(|p| at - p.time);
    after.into_iter().chain(before).fold(f64::INFINITY, f64::min)
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
        //
        // ...and over the same stretch. The two medians have to be arrived at
        // the same way to be worth comparing, and until now one of them was a
        // window and the other the whole recording. On broadcast material that
        // is a distinction without a difference -- a GOP is half a second
        // wherever you look -- but a disc puts an entry point at every scene
        // change, so a minute of cutting carries them twice as thickly as a
        // minute of dialogue. Over a fast-cut stretch the window's own median
        // fell below the recording's, and the whole reel was thrown to the
        // decoder although every cell of it stood on a picture already held:
        // on a Blu-ray episode read out of an ISO, one refresh in seven at
        // `GOP・6 秒`, costing 0.8 s each -- which at the fastest search speed
        // is the strip standing still while three quarters of a minute goes
        // past. See `examples/scixdiag.rs`.
        let gaps: Vec<f64> = times
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .filter(|d| *d > 1e-9)
            .collect();
        let gap = smartcut_core::thumbs::median_gap(&gaps).unwrap_or(f64::INFINITY);
        // The stretch the request covers, which is the stretch the held
        // pictures are asked about. Taken as the extremes rather than as the
        // ends, because a strip that walks the edited timeline hands over a
        // run that jumps.
        let (from, to) = times
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &t| (a.min(t), b.max(t)));
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
        //
        // ...and never as far as the next entry point, whichever side it lies.
        // A disc puts one at every scene change, and on a VC-1 Blu-ray they
        // come as close as two frames -- which is the slack itself, so a cell
        // asking for one of them was answered with the picture from the other:
        // a different shot entirely, under this one's caption. Half way to the
        // neighbour is the furthest a picture can be and still be nearer to
        // what was asked for than to anything else.
        let held: Option<Vec<Option<Shot>>> = {
            let guard = locked(&thumbs.0);
            guard
                .as_ref()
                .filter(|t| {
                    // The whole recording's spacing where the stretch is too
                    // short to hold two pictures to measure between -- which
                    // is a request finer than anything the track can answer,
                    // and is meant to fall through to a decode.
                    let held = t.spacing_over(from, to).unwrap_or(t.interval);
                    !exact.unwrap_or(false) && gap >= held * 0.9
                })
                .map(|track| {
                    let fd = src.video.frame_duration();
                    times
                        .iter()
                        .map(|&t| {
                            let tol = (fd * 2.0).min(to_neighbour(&src.points, t) / 2.0);
                            track.nearest(t).filter(|h| (h.time - t).abs() <= tol).map(|h| Shot {
                                url: shot_url(app, &h.jpeg),
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
                        url: shot_url(app, &s.jpeg),
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
                o.map(|s| Shot {
                    url: shot_url(app, &s.jpeg),
                    time: s.time,
                    kind: kind_of(&s, src, marks),
                })
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
///
/// Off unless 環境設定 says otherwise, which the environment seeds at startup
/// -- see [`prefs`].
fn proxy_wanted() -> bool {
    prefs::proxy()
}

/// Make the recording ready to look at: the thumbnail track and scene index,
/// and the proxy if one was asked for.
#[tauri::command]
async fn prepare(app: tauri::AppHandle) -> Result<PrepareInfo, String> {
    let (src, generation) = {
        let state = app.state::<Opened>();
        let guard = locked(&state.0);
        let src = guard.as_ref().ok_or("no file open")?.clone();
        (src, app.state::<Generation>().0.load(Ordering::SeqCst))
    };
    // Behind the rest of the machine: the proxy and the thumbnail track
    // between them are the longest thing that happens on opening a file, and
    // the one nobody is watching a frame at a time. See [`off_thread_behind`].
    off_thread_behind(move || {
        let thumb_opts = smartcut_core::ThumbOptions::default();
        let mut note = String::new();
        if proxy_wanted() {
            let mut opts = proxy::ProxyOptions::default();
            // A width settled in 環境設定 beats the engine's own, which is
            // what the environment and the default between them came to.
            // The engine still brings it down to something the stage can
            // show -- see `proxy::MAX_WIDTH`.
            if let Some(w) = prefs::proxy_width() {
                opts.width = w;
            }
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
    let kept = locked(&app.state::<Held>().0).as_mut().and_then(|ix| ix.track.take());
    if let Some(track) = kept {
        let info = track_info(&track, began.elapsed().as_secs_f64());
        if current() != generation {
            return Err("cancelled".to_string());
        }
        let index = index_info(app, src, true);
        *locked(&app.state::<Thumbs>().0) = Some(track);
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
    if let Some(head) = locked(&app.state::<Thumbs>().0).take() {
        let tail = std::mem::take(&mut track.thumbs);
        track.thumbs = head.thumbs;
        track.thumbs.extend(tail);
    }
    let info = track_info(&track, began.elapsed().as_secs_f64());
    let index = remember(app, src, Some(&track));
    *locked(&app.state::<Thumbs>().0) = Some(track);
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
    // pixels wide and there is no video at all, so half an hour of broadcast
    // comes to around forty megabytes -- which is why the count is large where
    // the proxy cache's is eight, and the index for one finished last week is
    // still worth having.
    //
    // Two gigabytes rather than one because a film's is no longer the same
    // size as a television programme's: the held pictures are now capped by
    // what they weigh rather than by how many there are, so a 2 h 20 m disc
    // title keeps every one of its entry points and comes to about a hundred
    // megabytes. A gigabyte would have held eight of those; two holds a
    // couple of dozen broadcast recordings as before, and does not throw a
    // film's away to make room for them.
    let _ = seek_index::prune(&dir, 32, 2 << 30);
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

/// Whether the recording a row names has gone from the disc.
///
/// A row holds the path it was added with, and nothing tells the list when
/// that name stops meaning anything: renaming the file in Explorer leaves the
/// row pointing at a name nothing answers to. The editor opened on it all the
/// same -- the window came up, the open behind it failed, and what was left
/// was an empty cut editor with a line of error text in it and nothing to
/// close it with but the cross. Asked before the window is built, so the
/// answer is a sentence in the list instead.
///
/// True only where *nothing* of the name is there, because a recording is
/// named three ways here and two of them are not paths the operating system
/// knows:
///
/// | named | what is on the disc |
/// |---|---|
/// | `/films/a.ts` | the file itself |
/// | `/films/disc.iso/BDMV/STREAM/00001.m2ts` | the image, one of its ancestors |
/// | `/films/BDAV/STREAM/00001.m2ts@0-2879` | the clip, before the `@` |
///
/// See [`smartcut_core::input::Input::parse`], which reads the same three
/// shapes properly. This does not: it opens nothing, and a name it cannot
/// account for is left alone. A recording that is there and unreadable is not
/// this question -- the open says that, and says why.
#[tauri::command]
fn clip_gone(path: String) -> bool {
    let named = std::path::Path::new(&path);
    if named.exists() {
        return false;
    }
    if let Some((clip, _)) = path.rsplit_once('@') {
        if std::path::Path::new(clip).exists() {
            return false;
        }
    }
    !named.ancestors().skip(1).any(|a| a.is_file())
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
        // The smallest this window may be dragged to, and it has to be a size
        // everything in it is still *on*: at 620 the plan panel and the two
        // buttons under it were off the bottom of the screen, with nothing to
        // say where they had gone. 760 is what the column comes to with the
        // picture at the smallest it is worth showing -- see the short-window
        // rules in `styles.css`. It was 720 before the playback row, which
        // is a row of buttons taller.
        .min_inner_size(900.0, 760.0)
        .center()
        // Out of sight until the size it was last left at is on it. A window
        // that is sized after it is up opens and then jumps. See [`geometry`].
        .visible(false)
        .build()
        .map_err(|e| e.to_string())?;
    // Centred again where there is nothing remembered, because the builder
    // centred a window of the default size and what is there now may be half
    // a screen wider. A window put back where it was left is left there.
    if !geometry::restore(&window, EDITOR) && !window.is_maximized().unwrap_or(false) {
        let _ = window.center();
    }
    let _ = window.show();
    app.state::<EditorUp>().0.store(true, Ordering::SeqCst);
    geometry::watch(&window, EDITOR);
    // The list has to know the editor has gone, whichever way it went -- OK,
    // キャンセル, or the title bar's cross. Said from here rather than from the
    // page, because the page going away is the thing being reported.
    //
    // **And playback has to be told, because nothing else tells it.** The two
    // threads [`play`] starts watch `Playing` and nothing else -- not the
    // window, which they have no handle on. Left running, the picture goes
    // to a window that is not there and the sound keeps coming out of the
    // machine: the audio thread is pacing itself against the sound card, so
    // it plays on for whatever was left of the recording, which is the rest
    // of the programme when the editor is closed near the start of one.
    // Reported from Windows as music coming from nowhere, and it was this.
    // `CloseRequested` as well as `Destroyed`, so the sound stops as the
    // window goes rather than after it.
    let teller = app.clone();
    window.on_window_event(move |event| {
        if matches!(
            event,
            tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
        ) {
            teller.state::<EditorUp>().0.store(false, Ordering::SeqCst);
            teller.state::<Playing>().0.store(0, Ordering::SeqCst);
        }
        if matches!(event, tauri::WindowEvent::Destroyed) {
            let _ = teller.emit("editor-closed", ());
            // 拡大表示 magnifies the picture this window was showing, so it
            // has nothing left to show. Closed from here rather than left to
            // the page, which is going away as this is read.
            if let Some(zoom) = teller.get_webview_window(ZOOM) {
                let _ = zoom.close();
            }
            // And the list comes back up, which is where whoever closed this
            // window is going next. Without it the editor simply vanished:
            // anything else that had been raised over these two windows in
            // the meantime was what the screen fell back to, and the list --
            // the window the OK button just handed the cuts to -- was left
            // wherever it had been buried. Raised for every way out, because
            // OK, キャンセル and the cross all mean the same thing here.
            //
            // A moment afterwards rather than here, and off this thread. The
            // window manager has its own answer to a window being destroyed
            // -- it hands the screen to whatever was in front of it before --
            // and that answer lands after this event does. Asking first means
            // asking to be overruled.
            if let Some(list) = teller.get_webview_window(MAIN) {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    let _ = list.unminimize();
                    let _ = list.set_focus();
                });
            }
        }
    });
    Ok(())
}

/// Open 拡大表示, in a window of its own.
///
/// A window rather than a panel in the editor. What it is for is looking at
/// the picture *closely* -- whether a frame is interlaced, whether a logo
/// edge is where it looked -- and the answer to that is a bigger view than a
/// corner of a screen already carrying a timeline, a strip and a plan. A
/// window can be dragged onto a second monitor, made as large as the question
/// needs and left open across recordings.
///
/// Smaller than the editor by a long way, and it opens near the middle where
/// nothing has been remembered. `async` for the reason [`open_editor`] is:
/// building a webview on the main thread is what WebView2 will not do.
#[tauri::command]
async fn open_zoom(title: String, app: tauri::AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(ZOOM) {
        // Already up: this is the menu being pressed twice, not a second one.
        let _ = w.set_title(&title);
        let _ = w.unminimize();
        let _ = w.set_focus();
        return Ok(());
    }
    let window = WebviewWindowBuilder::new(&app, ZOOM, WebviewUrl::App("zoom.html".into()))
        .title(title)
        .inner_size(560.0, 480.0)
        // Enough for the picture and the row under it. Anything narrower is
        // a window whose own controls are off the side of it.
        .min_inner_size(360.0, 320.0)
        .center()
        .visible(false)
        .build()
        .map_err(|e| e.to_string())?;
    if !geometry::restore(&window, ZOOM) && !window.is_maximized().unwrap_or(false) {
        let _ = window.center();
    }
    let _ = window.show();
    geometry::watch(&window, ZOOM);
    // The editor's menu carries a tick, and the keyboard toggles it. Both
    // answer to this, whichever way the window went: the menu item, the key,
    // or the cross on its own title bar.
    let teller = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            let _ = teller.emit("zoom-closed", ());
        }
    });
    Ok(())
}

#[tauri::command]
fn close_zoom(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(ZOOM) {
        let _ = w.close();
    }
}

/// Open 継ぎ目の編集, in a window of its own.
///
/// A window rather than a panel on the output settings screen, and for the
/// reason the cut editor is one: a transition is *done to* one join, not
/// settled once for the list, and what it needs is the thing a settings
/// screen has no room for -- the seconds either side of that join, on screen
/// and playing. Six rows saying `ディゾルブ 1.4 秒 Sine イン-アウト` cannot be
/// judged; a second and a half of the two recordings crossing can.
///
/// Left again with OK or キャンセル, as the editor is. What the settings become
/// afterwards is the list window's business.
///
/// `async` for the reason [`open_editor`] is: building a webview on the main
/// thread is the one thing WebView2 will not do.
#[tauri::command]
async fn open_cross(title: String, app: tauri::AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(CROSS) {
        let _ = w.set_title(&title);
        let _ = w.unminimize();
        let _ = w.set_focus();
        return Ok(());
    }
    let window = WebviewWindowBuilder::new(&app, CROSS, WebviewUrl::App("cross.html".into()))
        .title(title)
        .inner_size(1060.0, 800.0)
        // The settings column is a fixed width and the picture wants the
        // rest; below this the picture is smaller than the column beside it,
        // which is the wrong way round for a window that exists to show one.
        .min_inner_size(880.0, 640.0)
        .center()
        .visible(false)
        .build()
        .map_err(|e| e.to_string())?;
    if !geometry::restore(&window, CROSS) && !window.is_maximized().unwrap_or(false) {
        let _ = window.center();
    }
    let _ = window.show();
    app.state::<CrossUp>().0.store(true, Ordering::SeqCst);
    geometry::watch(&window, CROSS);
    // Whichever way it went -- OK, キャンセル, or the cross on the title bar --
    // the list has to know, and **playback has to be told**: the two threads
    // it runs watch `Playing` and have no handle on the window, so a run left
    // going plays its sound out of a window that is not there. The editor's
    // own close does this and for the same reason; see [`open_editor`].
    let teller = app.clone();
    window.on_window_event(move |event| {
        if matches!(
            event,
            tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
        ) {
            teller.state::<CrossUp>().0.store(false, Ordering::SeqCst);
            teller.state::<Playing>().0.store(0, Ordering::SeqCst);
        }
        if matches!(event, tauri::WindowEvent::Destroyed) {
            // The two recordings go with it. They are an open file each and
            // an index each, and nothing else in the program reads them.
            *locked(&teller.state::<Crossed>().0) = None;
            let _ = teller.emit("cross-closed", ());
            if let Some(list) = teller.get_webview_window(MAIN) {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    let _ = list.unminimize();
                    let _ = list.set_focus();
                });
            }
        }
    });
    Ok(())
}

/// Put the seam window's own title bar right, without raising it.
///
/// The title names the two clips the join sits between, and the window moves
/// between joins without being reopened -- the picker at the top of its
/// settings column is what does that. [`open_cross`] would retitle it too,
/// but it also raises and focuses the window, which is right for the button
/// that opens it and wrong for a picker being used inside it.
#[tauri::command]
fn retitle_cross(title: String, app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(CROSS) {
        let _ = w.set_title(&title);
    }
}

#[tauri::command]
fn close_cross(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(CROSS) {
        let _ = w.close();
    }
}

/// What the seam window learned about the two recordings it is to show.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CrossInfo {
    /// The rate the preview is walked at, which is the clip before the join's
    /// own: the joined file is written at the master's rate, and the clip
    /// before is the master unless somebody says otherwise.
    fps: f64,
    before_duration: f64,
    after_duration: f64,
    before_audio: bool,
    after_audio: bool,
}

/// Open the two recordings at one join.
///
/// Both of them, because a crossing is both of them: the picture at each
/// instant of it comes out of one and the other. A pass apiece, and on
/// anything but a transport stream not even that -- see [`scan_cached`],
/// which picks up an index that is already beside the recording or already
/// in this session's cache. The clip list has usually walked both of these
/// already, so this is normally two reads of a small file.
///
/// Asked again for the same pair, it answers without reopening: the window
/// asks on every change of the join being looked at, and a list of twelve
/// episodes is eleven joins over twelve recordings.
#[tauri::command]
async fn cross_load(
    before: String,
    after: String,
    app: tauri::AppHandle,
) -> Result<CrossInfo, String> {
    off_thread(move || {
        {
            let state = app.state::<Crossed>();
            let held = locked(&state.0);
            if let Some(pair) = held.as_ref() {
                if pair.before_path == before && pair.after_path == after {
                    return Ok(info_of_pair(pair));
                }
            }
        }
        let (before_src, _) = scan_cached(&app, &before)?;
        let (after_src, _) = scan_cached(&app, &after)?;
        let pair = CrossPair {
            before_path: before,
            after_path: after,
            before: before_src,
            after: after_src,
        };
        let info = info_of_pair(&pair);
        *locked(&app.state::<Crossed>().0) = Some(pair);
        Ok(info)
    })
    .await
}

fn info_of_pair(pair: &CrossPair) -> CrossInfo {
    CrossInfo {
        fps: pair.before.video.frame_rate,
        before_duration: pair.before.duration,
        after_duration: pair.after.duration,
        before_audio: pair.before.audio.is_some(),
        after_audio: pair.after.audio.is_some(),
    }
}

/// One join, as the window states it.
///
/// The four times are the two clips' own and they bound what is being
/// joined: where the clip before the join stops and where the clip after it
/// starts, after each one's own cuts. See [`smartcut_core::crossview::Seam`].
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SeamSpec {
    before_in: f64,
    before_out: f64,
    after_in: f64,
    after_out: f64,
    crossing: Crossing,
}

/// The previewed stretch, as the window needs to draw a scrubber over it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SeamSpan {
    /// How long the whole preview runs.
    seconds: f64,
    /// Where the crossing begins on the preview's own clock, and where it
    /// ends. Equal where the join is a plain cut.
    at: f64,
    ends: f64,
    /// What the crossing really takes off each clip, which is not always what
    /// it asked for: see [`smartcut_core::crossview::Seam::takes`].
    takes_before: f64,
    takes_after: f64,
}

/// Work the seam out against the two open recordings, and do something with
/// it. Everything below goes through here so that none of them can disagree
/// about which pair is open or what the seam over it is.
fn with_seam<T>(
    app: &tauri::AppHandle,
    spec: &SeamSpec,
    what: impl FnOnce(&smartcut_core::crossview::Seam) -> Result<T, String>,
) -> Result<T, String> {
    let state = app.state::<Crossed>();
    let held = locked(&state.0);
    let pair = held.as_ref().ok_or("no join open")?;
    what(&seam_over(&pair.before, &pair.after, spec))
}

/// The seam itself, over two recordings that are already open.
///
/// A function and not a closure because both halves of playback want one and
/// they are on different threads.
fn seam_over<'a>(
    before: &'a Source,
    after: &'a Source,
    spec: &SeamSpec,
) -> smartcut_core::crossview::Seam<'a> {
    smartcut_core::crossview::Seam {
        before,
        before_in: spec.before_in,
        before_out: spec.before_out,
        after,
        after_in: spec.after_in,
        after_out: spec.after_out,
        transition: spec.crossing.clone().into_transition(),
    }
}

/// How long the preview runs and where the crossing sits in it.
#[tauri::command]
fn cross_span(seam: SeamSpec, app: tauri::AppHandle) -> Result<SeamSpan, String> {
    with_seam(&app, &seam, |seam| {
        let window = seam.window(smartcut_core::crossview::LEAD);
        let (takes_before, takes_after) = seam.takes();
        Ok(SeamSpan {
            seconds: window.seconds,
            at: window.at,
            ends: window.ends,
            takes_before,
            takes_after,
        })
    })
}

/// One instant of the seam, composited.
///
/// What the window shows while nothing is playing: a pointer dragged over the
/// scrubber, a frame stepped. Each call seeks both recordings, which is what
/// makes it answer a pointer rather than a clock.
#[tauri::command]
async fn cross_shot(
    seam: SeamSpec,
    time: f64,
    width: u32,
    app: tauri::AppHandle,
) -> Result<Shot, String> {
    off_thread(move || {
        let jpeg = with_seam(&app, &seam, |s| {
            let window = s.window(smartcut_core::crossview::LEAD);
            smartcut_core::crossview::shot(s, &window, time, width).map_err(|e| e.to_string())
        })?;
        Ok(Shot { url: shot_url(&app, &jpeg), time, kind: String::new() })
    })
    .await
}

/// Play the seam back from `from`, as a stream of pictures, with its sound.
///
/// The picture half walks an even grid at the master's rate -- a crossing is
/// defined at instants of the *output*, and the two recordings have grids of
/// their own that need not agree -- and is paced against a wall clock, with
/// anything already late dropped rather than shown late. Exactly what
/// [`play`] does for one recording, and the pictures travel the same road:
/// down a channel as the JPEG's own bytes, the instant in front of them.
///
/// **The sound is the two clips in turn, not mixed.** A dissolve has both
/// pictures on screen at once; the sound of a join is the one clip and then
/// the other, which is what the cutter writes. See
/// [`smartcut_core::play_audio_across`], which keeps one device open across
/// the pair so the join does not click.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn cross_play(
    app: tauri::AppHandle,
    seam: SeamSpec,
    from: f64,
    width: u32,
    fps: f64,
    run: u64,
    frames: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>,
) -> Result<(), String> {
    // The window this plays for may have gone already: see [`CrossUp`].
    if !app.state::<CrossUp>().0.load(Ordering::SeqCst) {
        return Ok(());
    }
    let (before, after) = {
        let state = app.state::<Crossed>();
        let held = locked(&state.0);
        let pair = held.as_ref().ok_or("no join open")?;
        (pair.before.clone(), pair.after.clone())
    };
    app.state::<Playing>().0.store(run, Ordering::SeqCst);
    // Nothing has been heard of this run yet, and what the meter holds is the
    // end of the last one.
    app.state::<Meter>().0.clear();

    // Worked out once, here, because both halves play the same stretch: the
    // picture walks the window and the sound plays the two clips under it.
    let sound = seam_over(&before, &after, &seam)
        .window(smartcut_core::crossview::LEAD)
        .sound();
    let silent = before.audio.is_none() && after.audio.is_none();

    // Audio runs on its own thread and its own clock: it keeps a ring buffer
    // fed and the sound card paces itself. `Playing` is the one thing the two
    // sides share, so stopping either one stops both. See [`play`].
    let audio_handle = (!silent).then(|| {
        let level = app.state::<Vol>().0.clone();
        let meter = app.state::<Meter>().0.clone();
        let (a_src, b_src) = (before.clone(), after.clone());
        let audio_app = app.clone();
        std::thread::spawn(move || {
            let stop_app = audio_app.clone();
            // Either answer stops it: this run is no longer the one in force,
            // or the window it plays for has gone.
            let stop = move || {
                stop_app.state::<Playing>().0.load(Ordering::SeqCst) != run
                    || !stop_app.state::<CrossUp>().0.load(Ordering::SeqCst)
            };
            let parts = vec![
                smartcut_core::Heard {
                    src: &a_src,
                    ranges: vec![sound.before],
                    // Past the end of its own stretch where the playhead is
                    // already beyond the handover, which is how a part that
                    // has nothing left to play is skipped.
                    from: sound.before.0 + from,
                },
                smartcut_core::Heard {
                    src: &b_src,
                    ranges: vec![sound.after],
                    from: sound.after.0 + (from - sound.at).max(0.0),
                },
            ];
            if let Err(e) = smartcut_core::play_audio_across(&parts, &level, &meter, stop) {
                eprintln!("audio playback: {e}");
                // Otherwise this fails in total silence: a release build has
                // no console, and the picture half plays on regardless.
                let _ = audio_app.emit("audio-error", e.to_string());
            }
        })
    });

    tauri::async_runtime::spawn_blocking(move || {
        let playing = app.state::<Playing>();
        let up = app.state::<CrossUp>();
        let began = std::time::Instant::now();
        let gap = 1.0 / fps.max(1.0);
        let s = seam_over(&before, &after, &seam);
        let window = s.window(smartcut_core::crossview::LEAD);
        let outcome = smartcut_core::crossview::play(
            &s,
            &window,
            from,
            width,
            fps,
            |t| {
                if playing.0.load(Ordering::SeqCst) != run || !up.0.load(Ordering::SeqCst) {
                    return smartcut_core::Pace::Stop;
                }
                let out_t = (t - from).max(0.0);
                let due = std::time::Duration::from_secs_f64(out_t);
                let now = began.elapsed();
                if due > now {
                    std::thread::sleep(due - now);
                } else if out_t + 2.0 * gap < now.as_secs_f64() {
                    // Already more than two pictures late. Showing it would
                    // put the picture further behind the sound rather than
                    // catch it up: the sound plays at the card's own speed and
                    // waits for nothing. A skip costs the composite and no
                    // more. See [`play`], which says the same at length.
                    return smartcut_core::Pace::Skip;
                }
                smartcut_core::Pace::Show
            },
            |t, jpeg| {
                let mut body = Vec::with_capacity(8 + jpeg.len());
                body.extend_from_slice(&t.to_le_bytes());
                body.extend_from_slice(&jpeg);
                let _ = frames.send(tauri::ipc::InvokeResponseBody::Raw(body));
            },
        )
        .map_err(|e| e.to_string());

        let _ = playing
            .0
            .compare_exchange(run, 0, Ordering::SeqCst, Ordering::SeqCst);
        if let Some(h) = audio_handle {
            let _ = h.join();
        }
        let _ = app.emit("cross-play-ended", run);
        outcome
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The picture at `time`, at the size the recording itself holds it.
///
/// What 拡大表示 magnifies. The stage's picture will not do: it is scaled to
/// the width of the stage, and scaling far enough takes the comb out of
/// interlaced material -- which is the first thing somebody opens a
/// magnifier to look for. This is the one place in the program that asks for
/// a picture at its own size.
///
/// Every line of the recording, and no line resampled:
/// [`smartcut_core::shot_at`] caps the width it is given at the picture's own
/// display width, and the height that comes with that width is the coded
/// height. So 1440x1080 at 16:9 comes back 1920x1080 -- stretched across,
/// which is how it is meant to be looked at, and untouched down the lines,
/// which is where the comb is.
///
/// **From the recording, never from a proxy.** A proxy is re-encoded and
/// smaller, so its pictures cannot answer the question this window is open
/// for. Cloned, so a detection holding the recording does not hold this up.
#[tauri::command]
async fn zoom_shot(time: f64, app: tauri::AppHandle) -> Result<Shot, String> {
    off_thread(move || {
        let src = {
            let state = app.state::<Opened>();
            let guard = locked(&state.0);
            guard.as_ref().ok_or("no file open")?.clone()
        };
        let s = smartcut_core::shot_at(&src, time, u32::MAX).map_err(|e| e.to_string())?;
        Ok(Shot { url: shot_url(&app, &s.jpeg), time: s.time, kind: s.kind.to_string() })
    })
    .await
}

/// Whether a cut editor is on screen at this moment.
///
/// Asked by the list when it hears that one has closed, because that message
/// can be about a window that has already been replaced: close the editor and
/// open another row straight away, and the news of the first window going can
/// land after the second one is up. The list acting on it then would let go of
/// the row it had just opened, and the new window would sit there empty
/// waiting for a clip nobody was going to name. See the `editor-closed`
/// handler in `app.js`.
#[tauri::command]
fn editor_up(up: State<EditorUp>) -> bool {
    up.0.load(Ordering::SeqCst)
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

/// What the editor calls the pass while it runs, said in one place because
/// the summary that window writes afterwards calls it that too and a phase
/// named two ways is two phases on screen.
fn phase_index() -> &'static str {
    tr!("シーク用インデックス", "the seek index")
}

/// The same pass under the name the clip list gives it, which is the lane's
/// name and not this pass's: the list runs it for the pictures, and says so
/// over the lane and in the queue line. The row's own progress is the third
/// place that name is printed, so it is printed here rather than the
/// editor's -- "the seek index 4%" in a row headed 「サムネイルを作成中」
/// is the same two-phases-on-screen the other one avoids.
fn phase_pictures() -> &'static str {
    tr!("サムネイル", "Thumbnails")
}

/// The label the editor window goes by. One at a time: there is one opened
/// recording in [`Opened`], so a second editor would be a second view of the
/// first one's material with the first one's marks.
const EDITOR: &str = "editor";

/// The clip list, which Tauri labels for us: a window declared in the
/// configuration without a label of its own is `main`.
const MAIN: &str = "main";

/// 拡大表示, the small window that magnifies part of the picture the editor
/// is showing. One at a time, like the editor: it is about the one recording
/// that window has open.
const ZOOM: &str = "zoom";

/// 継ぎ目の編集, where a transition is set and looked at. One at a time: it is
/// opened on one join out of the list and left again with OK.
const CROSS: &str = "cross";

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
        Some("walk") => stop.walk.fetch_add(1, Ordering::SeqCst),
        Some("pics") => stop.pics.fetch_add(1, Ordering::SeqCst),
        Some("cm") => stop.cm.fetch_add(1, Ordering::SeqCst),
        Some("blank") => stop.blank.fetch_add(1, Ordering::SeqCst),
        Some("quiet") => stop.quiet.fetch_add(1, Ordering::SeqCst),
        _ => {
            stop.walk.fetch_add(1, Ordering::SeqCst);
            stop.pics.fetch_add(1, Ordering::SeqCst);
            stop.cm.fetch_add(1, Ordering::SeqCst);
            stop.blank.fetch_add(1, Ordering::SeqCst);
            stop.quiet.fetch_add(1, Ordering::SeqCst)
        }
    };
}

/// How many cores one of the clip list's background passes may decode with.
///
/// Every core while nobody is cutting anything: the list working through a
/// night's recordings on its own should have the machine, and the wide lanes
/// splitting it between them is the whole of the sharing needed.
///
/// A quarter of it while the cut editor is open -- half the machine, split
/// again between the wide lanes. A pass over a recording is a decoder on
/// every core, and the film strip asking for the picture under the pointer is
/// one more decode that has to come back inside a frame or two. This is the
/// trade that lets them all run at once: the pass takes longer, the pointer
/// keeps its picture. Zero is what libavcodec reads as "as many as this
/// machine has".
///
/// What the trade costs was measured on a four-core machine, a 24-minute
/// 1440x1080 MPEG-2 recording off BS through the flat-picture pass: 36
/// seconds on every core, 46 on two, 72 on one. So the quarter share is twice
/// the wall time -- which is the right way round for a pass nobody is
/// watching, and the wrong way round for one somebody pressed a button for.
/// See [`asked_for_threads`].
fn background_threads(app: &tauri::AppHandle) -> usize {
    if app.get_webview_window(EDITOR).is_none() {
        return 0;
    }
    (cores() / (2 * WIDE_LANES)).max(1)
}

/// ...and how many the cut editor's own detection may have, which is a
/// different question with a different answer.
///
/// Half the machine. The editor is open by definition here -- the button is
/// in it -- so the share [`background_threads`] would give is a quarter, and
/// on the machine measured above that is 72 seconds against 36 for a pass
/// somebody is sitting and watching the percentage of. Half is the middle of
/// that curve: 46 seconds, and the other half of the machine is exactly what
/// the film strip and the stage want while the wait goes on.
///
/// Not every core, for the same reason the background share is not every
/// core: the pictures this pass takes are taken from under the pointer.
fn asked_for_threads() -> usize {
    (cores() / 2).max(1)
}

fn cores() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
}

/// How many of the clip list's lanes decode at width, which is what the cores
/// have to be divided by. See [`BatchStop`], which has one count per lane and
/// five of them.
///
/// Three, not five: the walk touches no decoder at all -- it reads packet
/// headers -- and the silences decode sound, which libavcodec threads for
/// neither captions nor audio. What is left is the thumbnail pass, the
/// flat-picture pass and the logo half of the commercial detection, and those
/// are what contend for cores with the film strip.
const WIDE_LANES: usize = 3;

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

/// The cheap first look at a recording: what the container says about it,
/// without reading it. See [`ClipOutline`].
///
/// Costs one open and libavformat's probe -- tens of milliseconds -- so the
/// list asks this of every row it has just been handed, in order, before any
/// of the long passes start. `Err` rather than `None` where the file will not
/// open at all: that is worth saying on the row, and it is the one thing this
/// can find out that the passes behind it would only find out later.
#[tauri::command]
async fn clip_outline(path: String) -> Result<ClipOutline, String> {
    off_thread(move || {
        let o = smartcut_core::outline(&path).map_err(|e| e.to_string())?;
        Ok(ClipOutline {
            name: clip_name(&path),
            path: o.path.clone(),
            codec: o.video.codec.clone(),
            width: o.video.width,
            height: o.video.height,
            fps: o.video.frame_rate,
            duration: o.duration,
            frames: (o.duration * o.video.frame_rate).round().max(0.0) as u64,
            interlaced: o.video.interlaced(),
            has_audio: o.audio.is_some(),
            audio_channels: o.audio.as_ref().map_or(0, |a| a.channels),
            audio_sample_rate: o.audio.as_ref().map_or(0, |a| a.sample_rate),
            audio_bits: o.audio.as_ref().map_or(0, |a| a.bits),
            audio_tracks: audio_tracks_of(&o.audios),
        })
    })
    .await
}

/// Read one clip of the list and leave its seek index on disc.
///
/// The walk over the packets, and only that: where the lossless points are,
/// which of them can start a cut, whether the stream carries pulldown, and
/// how long the recording really is. Written down at once, so a later session
/// -- or the editor, opened on this clip a moment from now -- finds it made.
///
/// The key pictures are *not* decoded here. They used to be, in the same
/// call, and that put three quarters of the wait in front of the quarter that
/// makes a row real: a folder of an evening's recordings had its last row
/// showing a path and nothing else for minutes, when everything that row
/// needed had been readable since the fourth second. They are their own pass
/// now, behind every walk rather than behind each one; see [`clip_pictures`].
///
/// Deliberately shares nothing with the editing session -- no [`Opened`], no
/// [`Thumbs`], no [`Generation`] -- because it runs while another recording
/// is open and being cut.
/// `keeps` is only wanted where this ends up doing the pictures as well --
/// see [`ClipInfo::pictures`] -- and is what the poster is taken from then.
#[tauri::command]
async fn index_clip(
    path: String,
    keeps: Vec<(f64, f64)>,
    app: tauri::AppHandle,
) -> Result<ClipInfo, String> {
    off_thread_behind(move || index_clip_now(&path, &keeps, &app)).await
}

fn index_clip_now(
    path: &str,
    keeps: &[(f64, f64)],
    app: &tauri::AppHandle,
) -> Result<ClipInfo, String> {
    let began = std::time::Instant::now();
    // What the lane had been asked to stop before this pass existed is not
    // about this pass. See [`BatchStop`].
    let mine = app.state::<BatchStop>().walk.load(Ordering::SeqCst);
    let stopped = move || app.state::<BatchStop>().walk.load(Ordering::SeqCst) != mine;
    // Named by the lane as well as by the clip. The two index lanes run at
    // the same time on different recordings, and a recording can be in the
    // list twice -- so the path alone no longer says which row a number is
    // about.
    let say = |phase: &str, done: f64| {
        let _ = app.emit("clip-progress", (path.to_string(), "walk", phase.to_string(), done));
    };
    if stopped() {
        return Err("cancelled".into());
    }
    say(tr!("読み込み中", "Reading"), 0.0);
    // The walk over the packets is a quarter of the whole pass on a local
    // disc and more than that over a share -- a gigabyte a second is what a
    // disc gives, and this reads every byte of the recording. It used to say
    // nothing while it did, so the row sat at 読み込み中 0% for the first
    // several seconds and the work looked as though it had not begun. It has;
    // this is it saying so.
    let reading = tr!("読み込み中", "Reading");
    let on = |f: f64| say(reading, f);

    // See [`one_read`]: off unless it is asked for.
    if one_read() {
        return merged_clip_now(path, keeps, app, began, &on, &stopped);
    }

    let (src, held) = scan_cached_reporting(app, path, Some(&on))?;
    if stopped() {
        return Err("cancelled".into());
    }

    // The pictures, if an earlier session left any, are not looked at here:
    // [`clip_pictures`] reads them off the same file a moment from now, and
    // carrying a whole thumbnail track back through this call only for the
    // list to drop it would be tens of megabytes moved for nothing.
    let cached = held.is_some();
    // Written down now, with no pictures in it. The walk is the expensive
    // half and it is finished; leaving it unwritten until the pictures are
    // made would mean a list stopped part-way through had to walk every one
    // of these files again. A trackless index is a case the reader already
    // knows -- it is what one written by an interrupted session looks like --
    // and the pass behind this one fills it in.
    if !cached {
        remember(app, &src, None);
    }

    say(tr!("完了", "Done"), 1.0);
    Ok(clip_info_of(path, &src, cached, began.elapsed().as_secs_f64()))
}

/// What the list is told about a recording it has now read. Shared by the two
/// ways of reading one; see [`one_read`].
fn clip_info_of(path: &str, src: &Source, cached: bool, seconds: f64) -> ClipInfo {
    let rates = smartcut_core::fit::rates(src);
    ClipInfo {
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
        variable: src.video.variable_rate,
        has_audio: src.audio.is_some(),
        audio_channels: src.audio.as_ref().map_or(0, |a| a.channels),
        audio_sample_rate: src.audio.as_ref().map_or(0, |a| a.sample_rate),
        audio_bits: src.audio.as_ref().map_or(0, |a| a.bits),
        audio_tracks: audio_tracks_of(&src.audios),
        video_rate: rates.video,
        audio_rate: rates.audio,
        can_shrink: rates.can_shrink,
        index_name: src.index_name.to_string(),
        points: src.points.len(),
        unusable_points: src.points.iter().filter(|p| p.open_gop() && !p.droppable).count(),
        // Where the material actually begins; nothing before it decodes.
        first_point: src.points.first().map_or(0.0, |p| p.time),
        pictures: None,
        cached,
        seconds,
    }
}

/// Whether to read this recording once rather than twice, which nothing but
/// the environment now decides.
///
/// The two ways of reading a recording are both here -- the walk and the
/// pictures as two passes, or [`smartcut_core::scan_with_pictures`] doing
/// both in one -- and for a while which one ran was worked out from where the
/// recording lived. Measured against a real SMB share, that turned out not to
/// be worth doing.
///
/// The second read is nearly always free. The pictures follow the walk by
/// about a clip, so they read a recording the machine has just pulled through
/// and it comes back out of the page cache -- and a cache hit costs nothing
/// on a share either. On a 1.80 GB recording over cifs the two passes moved
/// 1.00x its size across the wire, the same as one pass. Only where the cache
/// cannot hold the working set does the split path cost anything at all, and
/// there it was 1.29x -- not the 2x the arithmetic suggests, because most of
/// the second read still hits. Against that, the split path has every row
/// real three to four times sooner, and it was the faster of the two on a
/// local disc to begin with.
///
/// So the list pipelines, always. What is left here is the switch, because
/// the one-read path is still the right answer for a narrow enough pipe or
/// recordings larger than memory, and because having both is what let the two
/// be measured against each other at all: `SMARTCUT_ONE_READ=1` turns it on.
fn one_read() -> bool {
    matches!(std::env::var("SMARTCUT_ONE_READ").as_deref(), Ok("1") | Ok("on") | Ok("yes"))
}

/// The walk and the pictures in one read, for a recording it would cost twice
/// to read twice. See [`one_read`].
fn merged_clip_now(
    path: &str,
    keeps: &[(f64, f64)],
    app: &tauri::AppHandle,
    began: std::time::Instant,
    on: &(dyn Fn(f64) + Sync),
    stopped: &(dyn Fn() -> bool + Sync),
) -> Result<ClipInfo, String> {
    let opts = smartcut_core::ThumbOptions {
        threads: background_threads(app),
        ..Default::default()
    };
    let (src, track) = smartcut_core::scan_with_pictures(path, &opts, Some(on), Some(stopped))
        // A pass that was asked to stop does not come back with what it had.
        // Said in the one word the list watches for, because a stop is not a
        // failure -- the row goes back to 解析待ち rather than red.
        .map_err(|e| if stopped() { "cancelled".to_string() } else { e.to_string() })?;
    if stopped() {
        return Err("cancelled".into());
    }
    remember(app, &src, Some(&track));
    let mut info = clip_info_of(path, &src, false, began.elapsed().as_secs_f64());
    // Empty is what the list sends for a clip nothing has been cut out of,
    // because before this pass it had no exact length or first picture to
    // work the ranges out from. It has both now.
    let whole = [(info.first_point, src.duration)];
    let keeps = if keeps.is_empty() { &whole[..] } else { keeps };
    info.pictures = Some(ClipPictures {
        poster: poster_of(&track, keeps)
            .or_else(|| track.thumbs.first())
            .map(|t| as_url(&t.jpeg)),
        scenes: track.scenes.len(),
        cached: false,
        seconds: 0.0,
    });
    Ok(info)
}

/// Decode the clip's key pictures and put them with its index.
///
/// The other half of what [`index_clip`] used to do in one call, run behind
/// every walk rather than behind each one. It is the expensive half -- around
/// four seconds a gigabyte against the walk's one, and on the machine's cores
/// rather than on its disc -- and nothing a row *says* waits on it: what it
/// gives is the row's own picture and the scene marks, and the scrub track
/// the editor would otherwise build on opening.
///
/// `keeps` is what survives this clip's cuts, in source time, so the picture
/// is taken from a part of the recording that is actually being kept. Nearly
/// always the whole of it; a clip out of a project file arrives already cut.
///
/// Costs a read and no decoding where an earlier session already made them.
#[tauri::command]
async fn clip_pictures(
    path: String,
    keeps: Vec<(f64, f64)>,
    app: tauri::AppHandle,
) -> Result<ClipPictures, String> {
    off_thread_behind(move || clip_pictures_now(&path, &keeps, &app)).await
}

fn clip_pictures_now(
    path: &str,
    keeps: &[(f64, f64)],
    app: &tauri::AppHandle,
) -> Result<ClipPictures, String> {
    let began = std::time::Instant::now();
    let mine = app.state::<BatchStop>().pics.load(Ordering::SeqCst);
    let stopped = move || app.state::<BatchStop>().pics.load(Ordering::SeqCst) != mine;
    if stopped() {
        return Err("cancelled".into());
    }

    let answer = |track: &smartcut_core::Track, cached| ClipPictures {
        poster: poster_of(track, keeps)
            .or_else(|| track.thumbs.first())
            .map(|t| as_url(&t.jpeg)),
        scenes: track.scenes.len(),
        cached,
        seconds: began.elapsed().as_secs_f64(),
    };

    // Already made, by this session's earlier run or by an earlier session.
    // Nothing is opened and nothing is decoded: the pictures come off the
    // index file they were written into.
    if let Some(track) = held_index(app, path).and_then(|mut ix| ix.track.take()) {
        return Ok(answer(&track, true));
    }

    // The walk has been over this file already, so its index is on disc and
    // this open is a read of that rather than another pass over the
    // recording -- which is what makes the two halves affordable as two.
    let (src, _) = scan_cached(app, path)?;
    let reporter = app.clone();
    let owned = path.to_string();
    let watcher = app.clone();
    let track = smartcut_core::thumbs::build_with(
        &src,
        &smartcut_core::ThumbOptions { threads: background_threads(app), ..Default::default() },
        Some(Box::new(move |f| {
            let _ =
                reporter.emit(
                    "clip-progress",
                    (owned.clone(), "pics", phase_pictures().to_string(), f),
                );
        })),
        // Nothing is looking at this recording, so there is nobody to hand
        // pictures to as they are made.
        None,
        Some(Box::new(move || {
            watcher.state::<BatchStop>().pics.load(Ordering::SeqCst) != mine
        })),
    )
    // A pass that was asked to stop does not come back with what it had; it
    // gives up where it stands and says so. Said in the one word the list
    // watches for, because a stop is not a failure -- the row keeps its index
    // and goes back to waiting for its pictures rather than turning red.
    .map_err(|e| if stopped() { "cancelled".to_string() } else { e.to_string() })?;
    // And a pass that was asked between finishing and returning is the same
    // thing. Writing a half-made track down would leave an index claiming to
    // hold pictures for the whole file, and every later session would believe
    // it.
    if stopped() {
        return Err("cancelled".into());
    }
    remember(app, &src, Some(&track));
    Ok(answer(&track, false))
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
    let dir = cache_kind(app, "proxy")?;
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
            if let Some(head) = locked(&app.state::<Thumbs>().0).take() {
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
    *locked(&app.state::<Held>().0) = None;
    *locked(&app.state::<Thumbs>().0) = Some(track);
    *locked(&app.state::<Proxy>().0) = Some(Proxied { src: psrc, marks });
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
        let mut guard = locked(&state.0);
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
fn hover_thumb(time: f64, thumbs: State<Thumbs>, app: tauri::AppHandle) -> Option<Shot> {
    let guard = locked(&thumbs.0);
    // `nearest` will hand back the last picture it has for any time past the
    // end of a track that is still being built -- the wrong picture, where
    // none is the honest answer.
    let t = guard.as_ref().filter(|t| time <= t.covered)?.nearest(time)?;
    Some(Shot { url: shot_url(&app, &t.jpeg), time: t.time, kind: "I".into() })
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
                let guard = locked(&thumbs.0);
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
    // The one thing 環境設定 has to say about a plan: whether a range that
    // begins on an open GOP is worth a second or two of re-encoding to get
    // off cleanly. Read here rather than passed in, so that the plan the
    // editor draws and the plan the export writes cannot disagree -- they
    // both come through this function.
    plan_on(src, ranges, &PlanOptions { clean_join: prefs::clean_join(), ..Default::default() })
}

#[tauri::command]
async fn make_plan(ranges: Vec<(f64, f64)>, app: tauri::AppHandle) -> Result<PlanInfo, String> {
    // The first plan for a recording may have to read its leading pictures,
    // which is a decode; see [`off_thread`].
    off_thread(move || plan_now(&ranges, &app)).await
}

fn plan_now(ranges: &[(f64, f64)], app: &tauri::AppHandle) -> Result<PlanInfo, String> {
    let state = app.state::<Opened>();
    let mut guard = locked(&state.0);
    let src = guard.as_mut().ok_or("no file open")?;
    if !src.leading_known {
        index::refine_leading(
            &src.input.url.clone(),
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
                &src.input.url.clone(),
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

/// One thing a clip of a join does not have in common with the master.
#[derive(Serialize)]
struct MismatchInfo {
    /// Which property differs, by a name that does not move with the prose:
    /// the window looks it up in its own language. See
    /// `smartcut_core::conform::What::slug`.
    what: String,
    /// What each of them says, as a reader wants it said.
    master: String,
    theirs: String,
}

/// What one clip of a join has to have done to it to go in beside the master.
#[derive(Serialize)]
struct FitInfo {
    /// The recording this is about. The window matches on it rather than on
    /// the position, because the answer is about a recording -- the same file
    /// twice in one list gets the same answer twice.
    path: String,
    /// Every picture of it decoded and written afresh at the master's shape.
    video: bool,
    /// At least one of its sound tracks the same.
    audio: bool,
    /// Why, so the window can say more than that it is happening. Empty for
    /// the master, which is the shape and cannot differ from it.
    mismatches: Vec<MismatchInfo>,
}

/// What each clip of a join has to have done to it to go in beside the
/// master.
///
/// **The one thing the output screen could not work out for itself.** A clip
/// cut on its own is copied bar the partial GOPs at the ends of its ranges,
/// and that is what [`clip_plan`] answers. A clip written into another
/// recording's shape has no copy available at any point of it -- every
/// picture is decoded and made again -- and the plan has nothing to say about
/// that. Without this the screen showed a seam or two, or said the whole clip
/// was being copied losslessly, over a run that was re-encoding an hour.
///
/// The judgement lives in `smartcut_core::conform` and only there, so that
/// what the window says and what the engine does cannot differ: this is the
/// window asking the same function `cut_into` asks.
#[tauri::command]
async fn join_fit(
    paths: Vec<String>,
    master: usize,
    app: tauri::AppHandle,
) -> Result<Vec<FitInfo>, String> {
    off_thread(move || {
        use smartcut_core::conform;
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        // Opened rather than answered from what the list already holds. What
        // counts as matching is a reading of the streams -- the pixel format,
        // the colour, the field order, every sound track -- and a row holds
        // only what a row shows. Every recording in the list has been indexed
        // by the time this is asked, so each of these opens picks up a seek
        // index instead of reading the file.
        let mut sources = Vec::with_capacity(paths.len());
        for path in &paths {
            sources.push(scan_cached(&app, path)?.0);
        }
        let master = master.min(sources.len() - 1);
        let shape = &sources[master];
        Ok(sources
            .iter()
            .enumerate()
            .map(|(n, src)| {
                let fit = if n == master {
                    conform::Fit::as_is(shape.audios.len())
                } else {
                    conform::fit(shape, src)
                };
                FitInfo {
                    path: paths[n].clone(),
                    video: fit.video,
                    audio: fit.audio.iter().any(|&a| a),
                    mismatches: if n == master {
                        Vec::new()
                    } else {
                        conform::compare(shape, src)
                            .iter()
                            .map(|m| MismatchInfo {
                                what: m.what.slug().to_string(),
                                master: m.master.clone(),
                                theirs: m.theirs.clone(),
                            })
                            .collect()
                    },
                }
            })
            .collect())
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
    /// What the broadcaster calls the track in its own words -- 日本語, 英語,
    /// 音声解説 -- where it named it at all.
    ///
    /// The language alone does not separate the two reasons a recording has
    /// a second sound track: a programme in two languages and a programme
    /// with commentary for a viewer who cannot see the picture both arrive
    /// as two tracks, and most of the commentary ones say Japanese on both.
    /// This is the field that tells the reader which one is in front of
    /// them. See [`smartcut_core::si::SoundTrack`].
    said: Option<String>,
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
/// Answered from the container alone, which is where every one of these
/// actually comes from: the streams, their PIDs, their languages, what each
/// one is, and what a cut cannot carry are all things the recording *names*.
/// None of it is in the access points, so none of it waits for the walk --
/// the menu opens on a recording nothing has read yet, in the time one open
/// takes. It used to go through `scan_cached`, which on a recording with no
/// index on disc meant **a second walk over a file already being walked** by
/// the window that was asking.
///
/// What it does not do is decide anything: which of these are written is the
/// window's answer, sent back with the export.
#[tauri::command]
async fn tracks(path: String) -> Result<Vec<StreamInfo>, String> {
    off_thread(move || {
        let src = smartcut_core::outline(&path).map_err(|e| e.to_string())?;
        let main = src.audio.as_ref().map(|a| a.stream_index);
        let mut out = Vec::new();
        for a in &src.audios {
            out.push(StreamInfo {
                index: a.stream_index,
                kind: "audio".into(),
                pid: a.pid,
                language: a.language.clone(),
                said: a.said.as_ref().and_then(|s| s.name.clone()),
                detail: format!("{} {}Hz {}ch", a.codec, a.sample_rate, a.channels),
                main: main == Some(a.stream_index),
                optional: true,
            });
        }
        for c in &src.captions {
            out.push(StreamInfo {
                index: c.stream_index,
                kind: match c.kind {
                    smartcut_core::TextKind::Caption => "caption",
                    smartcut_core::TextKind::Superimpose => "superimpose",
                }
                .into(),
                pid: c.pid,
                language: c.language.clone(),
                said: None,
                detail: match c.format {
                    smartcut_core::TextFormat::Arib => "ARIB STD-B24",
                    smartcut_core::TextFormat::Ttml => "ARIB-TTML",
                }
                .into(),
                main: false,
                optional: true,
            });
        }
        // The subtitles a disc draws rather than writes. A choice like the
        // rest: they travel, and a cut that does not want them can say so.
        for g in &src.graphics {
            out.push(StreamInfo {
                index: g.stream_index,
                kind: "graphics".into(),
                pid: g.pid,
                language: g.language.clone(),
                said: None,
                detail: "PGS".into(),
                main: false,
                optional: true,
            });
        }
        // A DVD's subtitles, which go into the file only if converted first:
        // where they end up is the output settings' question, and which of
        // them travel is answered in the chooser where a disc is opened
        // rather than here. See `smartcut_core::vobsub`.
        for s in &src.subpictures {
            out.push(StreamInfo {
                index: usize::MAX,
                kind: "subpicture".into(),
                pid: s.id,
                language: s.language.clone(),
                said: None,
                detail: "subpicture".into(),
                main: false,
                optional: false,
            });
        }
        // Listed but not offered: see `StreamInfo::optional`.
        for d in &src.dropped {
            out.push(StreamInfo {
                index: usize::MAX,
                kind: "dropped".into(),
                pid: d.pid,
                language: None,
                said: None,
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
            let guard = locked(&state.0);
            guard.as_deref() == Some(path.as_str())
        };
        if open_here {
            let state = app.state::<Thumbs>();
            let guard = locked(&state.0);
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
/// where there is one and the recording itself otherwise. It is a closure
/// rather than a recording because it is wanted at the *end* of a pass that
/// runs for minutes, and what is available then is not what was available at
/// the start; `None` leaves the blocks with the times they were found at. `say` carries the phase
/// and how far through it back to whoever asked, because the same pass
/// serves the editor's own button and the clip list's batch, and the two
/// report to different places.
fn detect_now(
    src: &Source,
    pictures: impl FnOnce() -> Option<Source>,
    threads: usize,
    inserts: bool,
    say: Say,
) -> Result<CmResult, String> {
    let opts = smartcut_core::DetectOptions {
        find_inserts: inserts,
        ..Default::default()
    };

    // The sound and the caption stream come out of one read of the
    // recording. Reading it is the cost on a share -- a caption stream is a
    // few hundred kilobytes of four gigabytes -- and these were two reads of
    // the whole file to find two small things in it.
    //
    // The logo is two more, and is much the larger share of the wait, so the
    // bar moves at an honest rate by giving it three quarters.
    const SOUND_SHARE: f64 = 0.25;
    let reporter = say.clone();
    let heard = smartcut_core::cm_silences_and_resets(
        src,
        &opts,
        Some(Box::new(move |f| {
            (*reporter)(
                tr!("音声と字幕を調べています", "Reading the sound and the captions"),
                f * SOUND_SHARE,
            )
        })),
    );
    // A recording with no sound to read still has marks worth having.
    let (silences, resets) = match heard {
        Ok(v) => v,
        Err(_) => (Vec::new(), smartcut_core::caption::resets(src)),
    };
    let resets = resets
        .ok()
        // And only where the station marks every junction rather than only
        // the places its programme stops and starts. A sparse marking is
        // exact about the few breaks it names and silent about the rest, and
        // read instead of the other two it emits one break where the
        // recording has four. See
        // [`smartcut_core::cm_marks_every_junction`].
        .filter(|r| smartcut_core::cm_marks_every_junction(r));

    let cands = smartcut_core::cm_candidates(&silences, &opts);

    // The logo is the better read on how far a break runs, but not every
    // broadcaster shows one; when it is missing, the silences stand alone.
    let logo = if resets.is_none() {
        let reporter = say.clone();
        smartcut_core::logo::detect_with(
            src,
            &smartcut_core::logo::LogoOptions {
                // The logo half spreads its decoding over the cores it is
                // allowed, which is the editor's share where the button was
                // pressed there and the background share where the list is
                // working through a queue. See [`asked_for_threads`].
                threads,
                ..Default::default()
            },
            Some(Box::new(move |f| {
                (*reporter)(
                    tr!("ロゴを探しています", "Looking for the logo"),
                    SOUND_SHARE + f * (1.0 - SOUND_SHARE),
                )
            })),
        )
        .ok()
    } else {
        None
    };
    (*say)(tr!("まとめています", "Putting it together"), 1.0);
    // What a boundary is put on the frame with, and what a junction is asked
    // about before a break's start is snapped to it.
    //
    // Asked for only now: see the note where this is passed in. The readings
    // above are the minutes of this pass, so by here a walk that had not
    // finished when it began has long since. Where there is still nothing --
    // no recording walked yet -- the blocks keep their estimates, and both
    // callers below make that check themselves.
    let pictures = pictures();
    let mut blocks = match (&resets, &logo) {
        (Some(r), _) => smartcut_core::cm_blocks_from_resets(r, src.duration),
        (None, Some(l)) if !l.absent.is_empty() => smartcut_core::cm_blocks_from_logo(
            &cands,
            &l.absent,
            &l.brief,
            &opts,
            3.0,
            src.duration,
            pictures.as_ref(),
        ),
        (None, Some(_)) => Vec::new(),
        _ => smartcut_core::cm_blocks(&cands, &opts, 0.6),
    };
    // The times a block arrives with are estimates -- the middle of a
    // silence, or the moment a logo's rolling average crossed a threshold.
    // Neither is a picture. The cut itself is a scene change, so a
    // boundary within reach of one is moved onto the exact frame it
    // happens on.
    if let Some(pictures) = &pictures {
        smartcut_core::cm_refine_boundaries(pictures, &mut blocks, 0.5, 0.08);
    }

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
    cache_kind(app, "cm")
}

/// Bumped when what is written here stops meaning what it used to -- a change
/// to the detection that would make yesterday's answer the wrong one. Every
/// cached detection is then ignored, and the recordings are read again.
///
/// 2: the caption resets stopped being read instead of the logo on stations
/// that mark only the ends of their breaks, breaks longer than two minutes
/// stopped being thrown away, and a mark from before the clock wrapped
/// stopped being read as a block at the head. Every recording those touch was
/// remembered with an answer this version would not give.
const CM_VERSION: u32 = 2;

/// Where this recording's commercial detection belongs.
fn cm_path(app: &tauri::AppHandle, src_path: &str) -> Result<std::path::PathBuf, String> {
    detection_path(app, src_path, "cm", "cmj", CM_VERSION)
}

/// Where a detection of `kind` kept on disc belongs.
///
/// Keyed the way [`seek_index::cache_path`] and [`proxy::cache_path`] key
/// theirs, and for the same reason: the path alone would go on answering for
/// a recording that has since been replaced, so the size and the modification
/// time are in the key and a changed file simply misses. `version` is in the
/// key as well, so a detection that has changed its mind about what it finds
/// does not read yesterday's answer.
fn detection_path(
    app: &tauri::AppHandle,
    src_path: &str,
    kind: &str,
    ext: &str,
    version: u32,
) -> Result<std::path::PathBuf, String> {
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
    eat(&version.to_le_bytes());
    eat(kind.as_bytes());

    let stem: String = std::path::Path::new(src_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .take(40)
        .collect();
    Ok(cache_kind(app, kind)?.join(format!("{stem}-{h:016x}.{ext}")))
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
        let _ = prune_detections(&dir, "cmj", 1000);
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
fn prune_detections(dir: &std::path::Path, ext: &str, keep: usize) -> std::io::Result<()> {
    let mut found: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some(ext) {
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
async fn detect_cm(path: String, inserts: bool, app: tauri::AppHandle) -> Result<CmResult, String> {
    // Reads the whole audio track, so it belongs off the UI thread -- and
    // runs for minutes, so it belongs behind the rest of the machine as well.
    off_thread_behind(move || {
        // What the three reading passes need is the recording's streams, and
        // the container names those. So this does not wait for the walk: on a
        // recording nothing has read yet the container's own answer is worn as
        // a `Source` and the captions, the audio and the logo are read out of
        // it exactly as they would be otherwise. See
        // [`smartcut_core::Outline::into_source`].
        //
        // Cloned rather than borrowed, and the lock let go before the pass
        // starts. This runs for minutes; holding [`Opened`] for that long
        // would stop the film strip and the stage, which read through the
        // same lock.
        let src = match opened_clone(&app, &path) {
            Some(src) => src,
            None => smartcut_core::outline(&path).map_err(|e| e.to_string())?.into_source(),
        };
        let reporter = app.clone();
        let watcher = app.clone();
        let owned = path.clone();
        let res = detect_now(
            &src,
            // Asked for at the end rather than at the start, because that is
            // when it is wanted and a great deal happens in between. A
            // detection begun while the recording was still being walked
            // finds the walk long finished by the time it has a boundary to
            // move: the reading is minutes and the walk is seconds. Where it
            // is somehow still not, the blocks keep the times they were found
            // at -- an estimate, and a better one than nothing.
            move || opened_clone(&watcher, &owned),
            asked_for_threads(),
            inserts,
            std::sync::Arc::new(move |phase: &str, done: f64| {
                let _ = reporter.emit("cm-progress", (phase.to_string(), done));
            }),
        )?;
        // Written down whichever window asked for it: it is the recording's
        // answer, not the window's, and the list is where it will be wanted
        // next.
        remember_cm(&app, &path, &res);
        Ok(res)
    })
    .await
}

/// A copy of the recording the editor has open, when that is `path`.
///
/// Copied so the caller can let the lock go: everything the editor draws
/// reads through it, and a pass that keeps it for minutes is a window that
/// has stopped redrawing for minutes.
///
/// The proxy first, where there is one, for the same reason [`with_pictures`]
/// prefers it -- refining a boundary is a picture comparison and nothing
/// more. `None` where nothing is open, or where what is open is a different
/// recording: the clip list asks about clips the editor is not on.
fn opened_clone(app: &tauri::AppHandle, path: &str) -> Option<Source> {
    {
        let state = app.state::<Proxy>();
        let guard = locked(&state.0);
        if let Some(p) = guard.as_ref() {
            if p.src.path == path {
                return Some(p.src.clone());
            }
        }
    }
    let state = app.state::<Opened>();
    let guard = locked(&state.0);
    guard.as_ref().filter(|s| s.path == path).cloned()
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
    off_thread_behind(move || {
        let mine = app.state::<BatchStop>().cm.load(Ordering::SeqCst);
        let stopped = || app.state::<BatchStop>().cm.load(Ordering::SeqCst) != mine;
        if stopped() {
            return Err("cancelled".into());
        }
        let (src, _) = scan_cached(&app, &path)?;
        let reporter = app.clone();
        let owned = path.clone();
        let refine = src.clone();
        let res = detect_now(
            &src,
            move || Some(refine),
            background_threads(&app),
            // The clip list has no screen of its own to ask on, and this is
            // an answer somebody gives while looking at one recording. See
            // [`smartcut_core::DetectOptions::find_inserts`].
            false,
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

/// One stretch the editor draws in its own lane: flat black, flat white, or
/// quiet.
///
/// The three arrive from two different passes and are the same shape on
/// purpose. What the editor does with them is the same thing -- a band under
/// the timeline and a mark at each end -- and a detection that came back in
/// its own shape would only have to be turned into this one.
#[derive(Serialize, Deserialize, Clone)]
struct FlatRun {
    /// `"black"`, `"white"` or `"quiet"`.
    kind: String,
    start: f64,
    end: f64,
    /// The last picture that is still flat, for 環境設定's answer about
    /// where the mark at the end of a stretch belongs. The same as `end` on
    /// a silence, which has no picture between the two: the sound comes back
    /// on a sample.
    ///
    /// Absent from every detection written before this was asked, and `end`
    /// is the answer those were made with.
    #[serde(default)]
    last: f64,
    /// How many pictures it holds, for the two that are about pictures. Zero
    /// for a silence, which is measured in samples and would be answering a
    /// question nobody asked.
    pictures: usize,
}

/// Where the picture goes flat, against the recording the editor has open.
///
/// Read afresh every time it is asked for, unlike [`flat_cached`]: the button
/// is how somebody says "look again", and the window has already shown
/// whatever was saved when it opened. What it finds is written down, so the
/// list and the next session get it for nothing.
///
/// 環境設定 has a say in three things here. `min_seconds` and `min_pictures`
/// are both applied, and the screen sets whichever of them the person chose a
/// unit for; `black` and `white` are which shades the pass is to look for,
/// and are what it is told rather than what its answer is filtered by -- so
/// what goes on disc is the question that was asked.
///
/// Half the machine, not all of it: the film strip this window is about wants
/// cores too, and the second half of them is worth about a third of this
/// pass's wall time. See [`asked_for_threads`].
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn detect_blank(
    path: String,
    min_seconds: f64,
    min_pictures: usize,
    black: bool,
    white: bool,
    levels: Levels,
    app: tauri::AppHandle,
) -> Result<Vec<FlatRun>, String> {
    off_thread_behind(move || {
        let src = flat_source(&app, &path)?;
        let reporter = app.clone();
        let say = move |done: f64| {
            let _ = reporter.emit("flat-progress", ("blank", done));
        };
        blank_now(
            &app,
            &src,
            &path,
            min_seconds,
            min_pictures,
            black,
            white,
            levels,
            asked_for_threads(),
            say,
        )
    })
    .await
}

/// Where the sound goes quiet, against the recording the editor has open.
///
/// The same silences commercial detection is built on -- there is one pass and
/// this is it -- reported as they come rather than ranked and grouped. Which is
/// the whole difference between the two answers: that one says where a break
/// is, this one says where it is quiet.
///
/// Cheap next to the pictures in processor time, and not in reading time: both
/// passes read the recording once, and over a share that is what either of them
/// costs. Measured on half an hour of broadcast off a NAS, 67 seconds for the
/// sound against 76 for the pictures.
#[tauri::command]
async fn detect_silence(
    path: String,
    threshold_db: f64,
    min_seconds: f64,
    app: tauri::AppHandle,
) -> Result<Vec<FlatRun>, String> {
    off_thread_behind(move || {
        let src = flat_source(&app, &path)?;
        let reporter = app.clone();
        let say = move |done: f64| {
            let _ = reporter.emit("flat-progress", ("quiet", done));
        };
        quiet_now(&app, &src, &path, min_seconds, threshold_db, say)
    })
    .await
}

/// Bumped when this pass stops meaning what it used to, as [`CM_VERSION`] is.
/// 1: as first written.
const FLAT_VERSION: u32 = 1;

/// A detection kept on disc: what it was found with, and what it found.
///
/// The two halves are written apart, because they are two passes and the
/// editor runs one at a time. So each file carries the answers its own pass
/// was given and says nothing about the other's: `min_pictures` belongs to the
/// pictures, the level belongs to the sound, and each leaves the other at
/// zero.
#[derive(Serialize, Deserialize)]
struct FlatSaved {
    min_seconds: f64,
    min_pictures: usize,
    /// Which shades the pictures pass was told to look for.
    ///
    /// Both where the file does not say, which is what every file written
    /// before the shades could be chosen holds: that pass looked for both.
    /// The sound's half carries them too and means nothing by them.
    #[serde(default = "looked_for")]
    black: bool,
    #[serde(default = "looked_for")]
    white: bool,
    /// ...and how dark, how bright and how much of the picture it was told to
    /// call flat. The engine's own answers where the file does not say, which
    /// is what every file written before these could be chosen holds -- they
    /// were the only answers then. The sound's half carries them and means
    /// nothing by them, as it does the shades.
    #[serde(default = "level_black")]
    black_level: f64,
    #[serde(default = "level_white")]
    white_level: f64,
    #[serde(default = "level_coverage")]
    coverage: f64,
    threshold_db: f64,
    runs: Vec<FlatRun>,
}

fn looked_for() -> bool {
    true
}

/// What a pass looks for when nobody has said: the engine's own answers, so
/// that the three screens and the three defaults cannot drift apart.
fn level_black() -> f64 {
    smartcut_core::BlankOptions::default().black_level
}
fn level_white() -> f64 {
    smartcut_core::BlankOptions::default().white_level
}
fn level_coverage() -> f64 {
    smartcut_core::BlankOptions::default().coverage
}

/// How dark, how bright, and how much of the picture: the three 環境設定 puts
/// on the pictures pass, carried together because they are answered together.
#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Levels {
    black_level: f64,
    white_level: f64,
    coverage: f64,
}


/// What both windows are told about a recording's flat stretches. Absent means
/// nothing has been detected, which is not the same as nothing being there.
#[derive(Serialize, Default)]
struct FlatFound {
    blank: Option<Vec<FlatRun>>,
    quiet: Option<Vec<FlatRun>>,
}

/// Where one half of this recording's flat detection belongs.
fn flat_path(
    app: &tauri::AppHandle,
    src_path: &str,
    quiet: bool,
) -> Result<std::path::PathBuf, String> {
    let ext = if quiet { "qtj" } else { "blkj" };
    detection_path(app, src_path, "flat", ext, FLAT_VERSION)
}

/// Write a half down, so the next window to ask does not read the recording
/// again. A failure is not worth stopping for, as with a detection.
fn remember_flat(app: &tauri::AppHandle, src_path: &str, quiet: bool, saved: &FlatSaved) {
    let Ok(file) = flat_path(app, src_path, quiet) else {
        return;
    };
    let Ok(json) = serde_json::to_vec(saved) else {
        return;
    };
    if let Err(e) = std::fs::write(&file, json) {
        eprintln!("flat: cannot write {}: {e}", file.display());
        return;
    }
    if let Some(dir) = file.parent() {
        let _ = prune_detections(dir, if quiet { "qtj" } else { "blkj" }, 1000);
    }
}

/// The half an earlier pass wrote for this recording, if the file is still the
/// one it was written for. Unreadable is deleted, as elsewhere.
fn saved_flat(app: &tauri::AppHandle, src_path: &str, quiet: bool) -> Option<FlatSaved> {
    let file = flat_path(app, src_path, quiet).ok()?;
    let raw = std::fs::read(&file).ok()?;
    match serde_json::from_slice::<FlatSaved>(&raw) {
        Ok(saved) => {
            seek_index::touch(&file);
            Some(saved)
        }
        Err(e) => {
            eprintln!("flat: discarding {}: {e}", file.display());
            let _ = std::fs::remove_file(&file);
            None
        }
    }
}

/// Whether a saved half answers what is being asked of it now.
///
/// A longer minimum than it was found with is answerable by leaving the short
/// runs out. A shorter one is not: the runs under the old minimum were never
/// written down. A level is not a minimum at all -- sound below -50 dB is not
/// a part of sound below -45 dB, it is a different question -- so the quiet
/// half answers only for the level it was read at.
///
/// The pictures' three levels are read the same way, and for a reason worth
/// stating: a stretch found at one threshold is not the same stretch found at
/// another even where both find it. What moves with the threshold is where a
/// fade is called black, which is exactly the end a mark is put on. So a saved
/// half answers only for the three it was read with, and a number typed in
/// 環境設定 sends every row back to the recording.
fn flat_answers(
    saved: &FlatSaved,
    min_seconds: f64,
    min_pictures: usize,
    db: Option<f64>,
    shades: Option<(bool, bool)>,
    levels: Option<Levels>,
) -> bool {
    if saved.min_seconds > min_seconds + 1e-9 || saved.min_pictures > min_pictures {
        return false;
    }
    if let Some(want) = levels {
        let same = |a: f64, b: f64| (a - b).abs() < 1e-9;
        if !same(saved.black_level, want.black_level)
            || !same(saved.white_level, want.white_level)
            || !same(saved.coverage, want.coverage)
        {
            return false;
        }
    }
    // A shade is like a minimum and not like a level: a pass that looked for
    // both answers for either of them alone, by leaving the other's stretches
    // out. A pass that looked for one cannot answer for the other -- those
    // pictures were never called anything -- so that is read again.
    if let Some((black, white)) = shades {
        if (black && !saved.black) || (white && !saved.white) {
            return false;
        }
    }
    match db {
        Some(db) => (saved.threshold_db - db).abs() < 1e-9,
        None => true,
    }
}

/// The runs of a saved half that clear the minimums being asked for now.
///
/// A silence has no pictures to count -- it is measured in samples, and
/// [`FlatRun::pictures`] is zero on one -- so the count is applied only where
/// there is one.
fn flat_keep(
    runs: &[FlatRun],
    min_seconds: f64,
    min_pictures: usize,
    shades: Option<(bool, bool)>,
) -> Vec<FlatRun> {
    let wanted = |kind: &str| match (shades, kind) {
        (Some((black, _)), "black") => black,
        (Some((_, white)), "white") => white,
        _ => true,
    };
    runs.iter()
        .filter(|r| {
            wanted(&r.kind)
                && r.end - r.start + 1e-9 >= min_seconds
                && (r.pictures == 0 || r.pictures >= min_pictures)
        })
        .cloned()
        .collect()
}

/// The recording the passes are to read: the one the editor has open where
/// that is this one, and the container's own answer otherwise.
///
/// Neither pass looks at the access points, so there is nothing a walk would
/// add -- and the copy is taken so the lock is let go before a pass that runs
/// for a minute. Both as [`detect_cm`] does it, and for its reasons.
fn flat_source(app: &tauri::AppHandle, path: &str) -> Result<Source, String> {
    match opened_clone(app, path) {
        Some(src) => Ok(src),
        None => Ok(smartcut_core::outline(path)
            .map_err(|e| e.to_string())?
            .into_source()),
    }
}

/// Run the pictures pass and write down what it found.
#[allow(clippy::too_many_arguments)]
fn blank_now(
    app: &tauri::AppHandle,
    src: &Source,
    path: &str,
    min_seconds: f64,
    min_pictures: usize,
    black: bool,
    white: bool,
    levels: Levels,
    threads: usize,
    say: impl FnMut(f64) + Send + 'static,
) -> Result<Vec<FlatRun>, String> {
    let opts = smartcut_core::BlankOptions {
        min_seconds,
        min_pictures,
        black,
        white,
        black_level: levels.black_level,
        white_level: levels.white_level,
        coverage: levels.coverage,
        threads,
        ..Default::default()
    };
    let runs: Vec<FlatRun> = smartcut_core::find_blank_runs_with(src, &opts, Some(Box::new(say)))
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|r| FlatRun {
            kind: r.shade.as_str().to_string(),
            start: r.start,
            end: r.end,
            last: r.last,
            pictures: r.pictures,
        })
        .collect();
    remember_flat(
        app,
        path,
        false,
        &FlatSaved {
            min_seconds,
            min_pictures,
            black,
            white,
            black_level: levels.black_level,
            white_level: levels.white_level,
            coverage: levels.coverage,
            threshold_db: 0.0,
            runs: runs.clone(),
        },
    );
    Ok(runs)
}

/// Run the sound pass and write down what it found.
fn quiet_now(
    app: &tauri::AppHandle,
    src: &Source,
    path: &str,
    min_seconds: f64,
    threshold_db: f64,
    say: impl FnMut(f64) + Send + 'static,
) -> Result<Vec<FlatRun>, String> {
    let opts = smartcut_core::DetectOptions {
        threshold_db,
        min_silence: min_seconds,
        ..Default::default()
    };
    let runs: Vec<FlatRun> = smartcut_core::find_silences_with(src, &opts, Some(Box::new(say)))
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|s| FlatRun {
            kind: "quiet".to_string(),
            start: s.start,
            end: s.end,
            last: s.end,
            pictures: 0,
        })
        .collect();
    remember_flat(
        app,
        path,
        true,
        &FlatSaved {
            min_seconds,
            min_pictures: 0,
            black: true,
            white: true,
            black_level: level_black(),
            white_level: level_white(),
            coverage: level_coverage(),
            threshold_db,
            runs: runs.clone(),
        },
    );
    Ok(runs)
}

/// What has already been detected about this recording, for a window that has
/// just opened one and for a row the list has just been handed.
///
/// Costs a `stat` and a read of a page or two, and answers with nothing in the
/// ordinary case where no detection has been made. Asked with the minimums in
/// force, because a saved half found with a shorter one answers for a longer
/// one and not the other way about; see [`flat_answers`].
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn flat_cached(
    path: String,
    min_seconds: f64,
    min_pictures: usize,
    black: bool,
    white: bool,
    levels: Levels,
    threshold_db: f64,
    quiet_seconds: f64,
    app: tauri::AppHandle,
) -> Result<FlatFound, String> {
    off_thread(move || {
        let mut out = FlatFound::default();
        let shades = Some((black, white));
        if let Some(saved) = saved_flat(&app, &path, false) {
            if flat_answers(&saved, min_seconds, min_pictures, None, shades, Some(levels)) {
                out.blank = Some(flat_keep(&saved.runs, min_seconds, min_pictures, shades));
            }
        }
        if let Some(saved) = saved_flat(&app, &path, true) {
            if flat_answers(&saved, quiet_seconds, 0, Some(threshold_db), None, None) {
                out.quiet = Some(flat_keep(&saved.runs, quiet_seconds, 0, None));
            }
        }
        Ok(out)
    })
    .await
}

/// The pictures pass over a recording nothing is looking at: one of the clip
/// list's two flat lanes.
///
/// A lane of its own rather than a part of commercial detection, which reads
/// the same recording and could have carried this. It is not folded in because
/// the cost is not alike: that detection reads the sound and the entry
/// pictures, and this reads every picture there is -- a minute and a quarter
/// on half an hour of broadcast against a quarter of that. Somebody who wants
/// the commercials found should not pay for the black frames as well.
///
/// And a lane apart from the sound's, for the same reason one step further in:
/// the two answer different questions, and this is the dear half. A list asked
/// for the silences alone has them in seconds and never decodes a picture.
///
/// A pass the cache already answers is not read again, which is what makes
/// asking the list for a recording the editor has already been over cost
/// nothing.
///
/// The one lane here that decodes at width, so it takes the background share
/// like the thumbnail pass and unlike the two detections that thread nothing:
/// every core while the editor is closed, a quarter of them while it is open.
/// See [`background_threads`].
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn detect_blank_at(
    path: String,
    min_seconds: f64,
    min_pictures: usize,
    black: bool,
    white: bool,
    levels: Levels,
    app: tauri::AppHandle,
) -> Result<Vec<FlatRun>, String> {
    off_thread_behind(move || {
        let mine = app.state::<BatchStop>().blank.load(Ordering::SeqCst);
        let stopped = || app.state::<BatchStop>().blank.load(Ordering::SeqCst) != mine;
        if stopped() {
            return Err("cancelled".into());
        }
        if let Some(saved) = saved_flat(&app, &path, false) {
            if flat_answers(
                &saved,
                min_seconds,
                min_pictures,
                None,
                Some((black, white)),
                Some(levels),
            ) {
                return Ok(flat_keep(
                    &saved.runs,
                    min_seconds,
                    min_pictures,
                    Some((black, white)),
                ));
            }
        }
        let src = flat_source(&app, &path)?;
        let reporter = app.clone();
        let owned = path.clone();
        let say = move |done: f64| {
            let _ = reporter.emit("clip-blank-progress", (owned.clone(), done));
        };
        let runs = blank_now(
            &app,
            &src,
            &path,
            min_seconds,
            min_pictures,
            black,
            white,
            levels,
            background_threads(&app),
            say,
        )?;
        if stopped() {
            return Err("cancelled".into());
        }
        Ok(runs)
    })
    .await
}

/// The sound pass over a recording nothing is looking at: the other flat lane.
///
/// Cheap where the pictures are dear -- the sound of half an hour of broadcast
/// is read in seconds off a local disc -- and not cheap over a share, where
/// both passes read the whole file and it is the network being measured: 67
/// seconds for the sound against 76 for the pictures on a NAS.
///
/// A recording with no sound is answered with the reason rather than with an
/// empty list. Nothing found and nothing to look at read the same on a row,
/// and they are not the same thing.
#[tauri::command]
async fn detect_quiet_at(
    path: String,
    threshold_db: f64,
    min_seconds: f64,
    app: tauri::AppHandle,
) -> Result<Vec<FlatRun>, String> {
    off_thread_behind(move || {
        let mine = app.state::<BatchStop>().quiet.load(Ordering::SeqCst);
        let stopped = || app.state::<BatchStop>().quiet.load(Ordering::SeqCst) != mine;
        if stopped() {
            return Err("cancelled".into());
        }
        if let Some(saved) = saved_flat(&app, &path, true) {
            if flat_answers(&saved, min_seconds, 0, Some(threshold_db), None, None) {
                return Ok(flat_keep(&saved.runs, min_seconds, 0, None));
            }
        }
        let src = flat_source(&app, &path)?;
        if src.audio.is_none() {
            return Err(tr!("音声がありません", "no audio").to_string());
        }
        let reporter = app.clone();
        let owned = path.clone();
        let say = move |done: f64| {
            let _ = reporter.emit("clip-quiet-progress", (owned.clone(), done));
        };
        let runs = quiet_now(&app, &src, &path, min_seconds, threshold_db, say)?;
        if stopped() {
            return Err("cancelled".into());
        }
        Ok(runs)
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
        .chain(src.captions.iter().map(|c| (c.pid, c.stream_index)))
        .chain(src.graphics.iter().map(|g| (g.pid, g.stream_index)));
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

/// Which of the containers the output settings screen offers can hold this
/// list of recordings.
///
/// The engine answers rather than the window, for the same reason it answers
/// what the encoders will write: what a container holds is a property of the
/// muxers this build was linked against, and a copy of the table kept on the
/// screen would drift from them. See [`smartcut_core::carry`].
///
/// `want` is the screen's own list of containers, so one added to the window
/// is one asked about. `video` and `audio` are the codecs in the list, each
/// named once. `asked` is the codec the screen wants the sound written in,
/// where it wants one at all: then the recordings' own sound is not what
/// will be in the file, and asking about it would grey a container out over
/// a codec the cut is not going to write.
///
/// Answered on this thread. It is a few string comparisons and the screen is
/// waiting on it to draw.
#[tauri::command]
fn containers_holding(
    want: Vec<String>,
    video: Vec<String>,
    audio: Vec<String>,
    asked: Option<String>,
) -> Vec<String> {
    use smartcut_core::carry::{family, holds};
    want.into_iter()
        .filter(|name| {
            let into = family(&format!("x.{name}"));
            // Linear PCM is the one answer that depends on where it is
            // going: a transport stream declares Blu-ray's own and
            // everything else takes the plain samples. See `carriage` in
            // `smartcut_core::cut`.
            let sound: Vec<String> = match asked.as_deref().unwrap_or("") {
                "" | "source" => audio.clone(),
                "lpcm" => vec![if into == "ts" { "pcm_bluray" } else { "pcm_s16be" }.to_string()],
                named => vec![named.to_string()],
            };
            video.iter().chain(sound.iter()).all(|c| holds(into, c))
        })
        .collect()
}

/// A name for the folder a run makes that none of `dirs` already has.
///
/// A run writing into a folder of its own makes that folder as the first cut
/// is written, and one of the same name already there would have this run's
/// files laid over the last one's -- same list, same prefix, same numbering,
/// so the names land on top of each other and the earlier run is gone without
/// a word. The name is branched instead: `night`, then `night-2`, `night-3`,
/// which is the count [`free_in`] gives a queued project's copy, and for the
/// same reason.
///
/// Asked about every folder the run will write into rather than one. With no
/// output folder chosen the cuts go beside the recordings they were made
/// from, so a list gathered out of three folders is three of these folders;
/// one answer for the whole list is what the screen shows and what the run
/// writes, so the branch has to be free in all of them.
///
/// Never asked for a disc. A second run onto one adds to it -- that is how a
/// disc gets filled over a week -- so a folder already there is the point
/// rather than the problem. See [`bdav_prepare`].
#[tauri::command]
async fn free_folder(dirs: Vec<String>, name: String) -> Result<String, String> {
    // A `stat` per folder tried, against paths somebody chose. See
    // [`resolve_paths`].
    off_thread(move || free_folder_now(&dirs, &name)).await
}

fn free_folder_now(dirs: &[String], name: &str) -> Result<String, String> {
    // Shares resolved the way they are everywhere else. One that is not
    // connected is left out rather than refused: the run is about to fail on
    // it with a sentence of its own, and a folder nothing can look at is not
    // a folder that is in the way.
    let ats: Vec<std::path::PathBuf> = dirs.iter().filter_map(|d| local_path(d).ok()).collect();
    for n in 1..1000 {
        let branched = if n == 1 { name.to_string() } else { format!("{name}-{n}") };
        if ats.iter().all(|at| !at.join(&branched).exists()) {
            return Ok(branched);
        }
    }
    Err(trf!(
        "フォルダーの名前が付けられません: {name}",
        "Cannot find a free folder name: {name}"
    ))
}

/// What one recording of a list costs a disc, as the window knows it.
///
/// The rates came off the recording when it was indexed and travel with the
/// row ([`ClipInfo::video_rate`]); the seconds are how much of it the cuts
/// keep, which the window works out itself as the ranges move. So this is
/// arithmetic on numbers the window already has, and it can be asked as often
/// as somebody drags a cut.
#[derive(serde::Deserialize)]
struct ClipCost {
    seconds: f64,
    video_rate: f64,
    audio_rate: f64,
    can_shrink: bool,
}

/// What one of them is expected to come to, for the gauge to draw.
#[derive(serde::Serialize)]
struct ClipRoom {
    bytes: u64,
    video_bytes: u64,
    can_shrink: bool,
}

/// And what the list comes to against the disc.
#[derive(serde::Serialize)]
struct DiscRoom {
    clips: Vec<ClipRoom>,
    bytes: u64,
    video_bytes: u64,
    capacity: u64,
    usable: u64,
    /// What share of their own size the pictures would have to be written at.
    share: f64,
    fits: bool,
    reachable: bool,
    /// The least a picture may be asked to come to, so the window can say why
    /// a list that will not fit will not fit.
    floor: f64,
}

/// What a night's cuts come to against a disc, and what has to come off.
///
/// See `smartcut_core::fit`. Nothing here opens a file.
#[tauri::command]
fn disc_room(clips: Vec<ClipCost>, capacity: u64, margin: f64) -> DiscRoom {
    use smartcut_core::fit;
    let estimates: Vec<fit::Estimate> = clips
        .iter()
        .map(|c| {
            fit::estimate_at(
                fit::Rates {
                    video: c.video_rate,
                    audio: c.audio_rate,
                    can_shrink: c.can_shrink,
                },
                c.seconds,
                fit::Going::Disc,
            )
        })
        .collect();
    let f = fit::fit(&estimates, capacity, margin);
    DiscRoom {
        clips: estimates
            .iter()
            .map(|e| ClipRoom {
                bytes: e.bytes,
                video_bytes: e.video_bytes,
                can_shrink: e.can_shrink,
            })
            .collect(),
        bytes: f.bytes,
        video_bytes: f.video_bytes,
        capacity: f.capacity,
        usable: f.usable,
        share: f.share,
        fits: f.fits,
        reachable: f.reachable,
        floor: fit::FLOOR,
    }
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
    // Where the subtitles a disc draws go: "pgs" puts them inside the cut,
    // as the kind a transport stream carries; "beside" writes the .idx and
    // .sub pair next to it; "sup" writes the display sets themselves into a
    // .sup next to it. Nothing sent means inside, which is one file rather
    // than two.
    subtitles: Option<String>,
    // Streams switched off in the chooser when a disc was read, by PID.
    //
    // A PID and not an index because the chooser answers before anything is
    // open: the disc's index names a track by the PID it sits on, and a
    // stream index is a thing libavformat makes up once it has read the
    // recording. Resolved here, where the recording *is* open.
    drop_pids: Option<Vec<i32>>,
    // Whether the recording's data broadcast travels with the cut. Nothing
    // sent leaves it to the engine, which carries it wherever it can be
    // carried -- a project written before the box existed is one nobody
    // said no on.
    data_broadcast: Option<bool>,
    // What share of their own size the pictures are written back at, where
    // the run has to fit a disc. One number for the whole list, worked out
    // once before the run starts; see `smartcut_core::fit`.
    video_share: Option<f64>,
) -> Result<(), String> {
    // Cutting is minutes of I/O on a broadcast recording; keeping it off the
    // UI thread is what lets the progress bar move at all.
    tauri::async_runtime::spawn_blocking(move || {
        // The output folder is typed as often as it is picked, so it too can
        // name a share: `smb://nas/rec` lands on the mount the same way an
        // input does. A plain path is handed back untouched.
        let output = local_path(&output)?.to_string_lossy().into_owned();
        // The folder the cut goes in may not exist yet -- a run writing into
        // a folder of its own makes one that was named on the settings
        // screen and has never been anywhere else. Made here rather than
        // before the run, so that a folder is made for a cut that is
        // actually about to be written and not for one that failed to open.
        if let Some(dir) = std::path::Path::new(&output).parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("making {}: {e}", dir.display()))?;
        }
        // Owned either way, so the recording's lock is not held for the
        // minutes the cut takes: the editor has to keep answering while its
        // own output runs.
        let mut src = match &path {
            Some(p) => scan_cached(&app, p)?.0,
            None => locked(&app.state::<Opened>().0)
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
                &src.input.url.clone(),
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
            subtitles: match subtitles.as_deref() {
                Some("beside") => smartcut_core::cut::Subtitles::Beside,
                Some("sup") => smartcut_core::cut::Subtitles::Sup,
                _ => smartcut_core::cut::Subtitles::Pgs,
            },
            drop_streams: streams_to_drop(&src, drop_streams, drop_pids.clone()),
            // A DVD's subtitles are named by their substream id, which is
            // what the chooser sends: they have no stream index to be named
            // by. See `smartcut_core::SubpictureInfo`.
            drop_subpictures: drop_pids
                .unwrap_or_default()
                .into_iter()
                .filter(|pid| src.subpictures.iter().any(|s| s.id == *pid))
                .collect(),
            data_broadcast,
            video_share,
            // 環境設定, like the joins and the proxy: it is a standing answer
            // about how this program cuts rather than something one project
            // decides, and the output screen is already the longest screen
            // in the program.
            audio_fade: prefs::audio_fade(),
            ..Default::default()
        };
        smartcut_core::cut_with_progress(
            &src,
            &plans,
            &output,
            &opts,
            // Which of the two passes, as well as how far: the second is a
            // read and a write of the whole finished file, and a window that
            // says 出力中 through it is a window saying the wrong thing for
            // a third of the run. See [`smartcut_core::cut::Pass`].
            //
            // Two figures, and the screen uses them for two different things:
            // `done` runs once across both passes and is what the bar is
            // drawn from, `within` is how far through its own pass this is
            // and is what the stage follows. Only the first pass has
            // pictures being written; see `followWrite`.
            Some(Box::new(move |pass, done, within| {
                let tables = pass == smartcut_core::cut::Pass::Tables;
                let _ = reporter.emit("export-progress", (whose.clone(), tables, done, within));
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

/// One clip of a list being written into a single file.
///
/// What the window sends per clip, which is everything about it that is not
/// an answer for the whole run: where it is, what is kept of it, which of
/// its own streams it wants written, and what happens where it gives way to
/// the clip after it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JoinClip {
    path: String,
    ranges: Vec<(f64, f64)>,
    drop_streams: Option<Vec<usize>>,
    drop_pids: Option<Vec<i32>>,
    /// The transition that follows this clip, as the screen holds it. See
    /// [`smartcut_core::transition`].
    after: Option<Crossing>,
}

/// A transition as the window states one.
///
/// `Clone` because the seam window asks about the same one repeatedly -- a
/// picture a frame, a span per change -- where the export consumes it once.
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Crossing {
    /// The name the engine parses -- `dissolve`, `wipe-left`. Anything it
    /// does not know is no transition at all, which is what a project file
    /// written by a later version comes to here.
    kind: String,
    seconds: f64,
    curve: String,
    mode: String,
    /// A still laid over the crossing.
    image: Option<String>,
}

impl Crossing {
    fn into_transition(self) -> smartcut_core::transition::Transition {
        smartcut_core::transition::Transition {
            kind: smartcut_core::transition::Crossing::parse(&self.kind).unwrap_or_default(),
            seconds: self.seconds,
            easing: smartcut_core::transition::Easing::parse(&self.curve, &self.mode),
            overlay: self.image.filter(|p| !p.is_empty()),
        }
    }
}

/// Write a list of clips out as one file.
///
/// The counterpart of [`export`] for a run the output screen is writing as a
/// single output rather than one per row. Everything that describes the
/// output is settled once -- the sound, the subtitles, the tables -- and
/// what is per clip is what the list holds about it.
///
/// **The reporting is the one thing that could not be shared.** `export`
/// tags its fraction with the recording it belongs to, because the output
/// screen is running through a list and an untagged fraction would move
/// whichever row was on screen. Here there is one job and one bar, so the
/// tag is the first clip's -- the row the run is drawn on.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn export_joined(
    app: tauri::AppHandle,
    clips: Vec<JoinClip>,
    master: usize,
    output: String,
    audio_reencode: Option<bool>,
    audio_copy: Option<bool>,
    audio_codec: Option<String>,
    audio_channels: Option<u16>,
    audio_bitrate: Option<usize>,
    audio_sample_rate: Option<u32>,
    audio_bits: Option<u8>,
    audio_es: Option<bool>,
    subtitles: Option<String>,
    data_broadcast: Option<bool>,
    video_share: Option<f64>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        if clips.is_empty() {
            return Err("nothing to write: the list is empty".to_string());
        }
        let output = local_path(&output)?.to_string_lossy().into_owned();
        if let Some(dir) = std::path::Path::new(&output)
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
        {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("making {}: {e}", dir.display()))?;
        }
        // Every recording opened and planned before the first byte, because
        // the shape of the output depends on all of them: which sound tracks
        // are written afresh is an answer over the whole list. See
        // `smartcut_core::conform`.
        let mut sources = Vec::with_capacity(clips.len());
        for clip in &clips {
            let mut src = scan_cached(&app, &clip.path)?.0;
            if !src.leading_known {
                index::refine_leading(
                    &src.input.url.clone(),
                    &src.video.clone(),
                    src.start_time,
                    &mut src.points,
                    &clip.ranges,
                )
                .map_err(|e| e.to_string())?;
            }
            sources.push(src);
        }
        let plans: Vec<Vec<smartcut_core::RangePlan>> = sources
            .iter()
            .zip(&clips)
            .map(|(src, clip)| build_plan(src, &clip.ranges))
            .collect();
        // Which of them the file takes its shape from, and whose streams it
        // is declared with. Read off the list before the clips are consumed
        // below: the tracks the output declares are the master's, and a
        // stream index means nothing outside the file it came from.
        let master = master.min(clips.len() - 1);
        let by_index = clips[master].drop_streams.clone();
        let by_pid = clips[master].drop_pids.clone();
        let afters: Vec<smartcut_core::transition::Transition> = clips
            .into_iter()
            .map(|c| c.after.map(Crossing::into_transition).unwrap_or_default())
            .collect();
        let reels: Vec<smartcut_core::cut::Reel> = sources
            .iter()
            .zip(&plans)
            .zip(afters)
            .map(|((src, plans), after)| smartcut_core::cut::Reel { src, plans, after })
            .collect();
        let whose = reels[0].src.path.clone();
        let reporter = app.clone();
        let opts = smartcut_core::CutOptions {
            audio_mode: if audio_reencode.unwrap_or(false) {
                smartcut_core::AudioMode::Reencode
            } else if audio_copy.unwrap_or(false) {
                smartcut_core::AudioMode::Copy
            } else {
                smartcut_core::AudioMode::Smart
            },
            audio_codec: audio_codec
                .as_deref()
                .and_then(smartcut_core::AudioCodec::parse)
                .unwrap_or_default(),
            audio_channels: audio_channels.filter(|&c| c > 0),
            audio_bit_rate: audio_bitrate.filter(|&b| b > 0),
            audio_sample_rate: audio_sample_rate.filter(|&r| r > 0),
            audio_bits: audio_bits.filter(|&b| b > 0),
            subtitles: match subtitles.as_deref() {
                Some("beside") => smartcut_core::cut::Subtitles::Beside,
                Some("sup") => smartcut_core::cut::Subtitles::Sup,
                _ => smartcut_core::cut::Subtitles::Pgs,
            },
            // The master's own, since the master is the reel the output is
            // declared from and the only one whose stream indices mean
            // anything to it.
            drop_streams: streams_to_drop(reels[master].src, by_index, by_pid),
            data_broadcast,
            video_share,
            audio_fade: prefs::audio_fade(),
            // The planner's own settings, because a transition is planned
            // inside the engine and what is left of the range it takes from
            // has to be planned again the way this window plans one. See
            // `build_plan`.
            plan: PlanOptions {
                clean_join: prefs::clean_join(),
                ..Default::default()
            },
            ..Default::default()
        };
        smartcut_core::cut::join_with_progress(
            &reels,
            master,
            &output,
            &opts,
            Some(Box::new(move |pass, done, within| {
                let tables = pass == smartcut_core::cut::Pass::Tables;
                let _ = reporter.emit("export-progress", (whose.clone(), tables, done, within));
            })),
        )
        .map_err(|e| e.to_string())?;

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

/// Where one recording is to be written on a disc being built.
#[derive(Serialize)]
struct BdavSlot {
    /// The five digits the recording's three files share.
    clip: String,
    /// The file the cut is to be written into, which is the disc's to name.
    path: String,
}

/// What the disc's index will say about one recording it holds.
#[derive(Deserialize)]
struct BdavEntry {
    clip: String,
    name: String,
    /// `2026-08-17 01:00:00`, or nothing where the recording never said.
    made: Option<String>,
    /// How long the programme ran on air, in seconds. Carried rather than
    /// typed: it is the listing's own length.
    ran: Option<u32>,
    /// What the broadcaster said the programme was, and the channel it came
    /// off: the rest of what a recorder writes into a playlist.
    description: Option<String>,
    channel: Option<String>,
    channel_number: Option<u16>,
    /// Chapter points, in seconds on the written stream's own timeline.
    marks: Vec<f64>,
}

/// What a recording says about the programme in it.
#[derive(Serialize)]
struct ProgrammeInfo {
    name: Option<String>,
    description: Option<String>,
    channel: Option<String>,
    channel_number: u16,
    made: Option<String>,
    /// How long the programme ran on air, in seconds.
    ran: Option<u32>,
}

/// Read what a recording says about its own programme.
///
/// For the output settings screen, which shows what would be written into a
/// disc's index beside each recording -- and for the export, which writes
/// it. A recording that says nothing about itself is not an error: plenty of
/// files have been through tools that kept none of it.
#[tauri::command]
async fn programme(path: String) -> Result<ProgrammeInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let input = smartcut_core::input::Input::parse(&path).map_err(|e| e.to_string())?;
        let said = smartcut_core::si::programme(&input, 0).unwrap_or_default();
        Ok(ProgrammeInfo {
            name: said.name,
            description: said.description,
            channel: said.channel,
            channel_number: said.channel_number,
            made: said.began.map(|m| m.to_string()),
            ran: said.ran,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What a disc holding these recordings is called, when nobody has typed a
/// name for it: the series they are episodes of.
///
/// Worked out here rather than on the screen so that the disc the export
/// writes and the name the settings screen shows are the same answer, out of
/// the same rules -- see [`smartcut_core::series`]. Empty where the names
/// say nothing, which leaves the screen to fall back on its own.
#[tauri::command]
fn series_title(names: Vec<String>) -> String {
    smartcut_core::series::shared(&names).unwrap_or_default()
}

/// Make the disc's directories and say what the next `n` recordings on it
/// are to be called.
///
/// Asked before the run rather than during it, because the numbering depends
/// on what the disc already holds: a second evening's cuts are added to the
/// disc rather than written over it.
#[tauri::command]
async fn bdav_prepare(dir: String, n: usize) -> Result<Vec<BdavSlot>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let at = local_path(&dir)?;
        let clips = smartcut_core::bdav::prepare(&at, n).map_err(|e| e.to_string())?;
        Ok(clips
            .into_iter()
            .map(|clip| BdavSlot {
                path: smartcut_core::bdav::stream_of(&at, &clip).to_string_lossy().into_owned(),
                clip,
            })
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Take back the streams of the recordings a run did not finish.
///
/// A slot is reserved before the cut starts, so a run that is stopped part
/// way -- or one whose cut fails -- leaves a stream on the disc that nothing
/// names: no clip index, no playlist, and the next run numbers past it. It is
/// gigabytes of a recording that was never finished, and it would go into
/// any image made of the disc afterwards. Only the numbers this run was given
/// are passed here, and only `BDAV/STREAM/00001.m2ts` under the disc's own
/// folder is built from one, so nothing outside the disc can be named.
#[tauri::command]
async fn bdav_discard(dir: String, clips: Vec<String>) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let at = local_path(&dir)?;
        let mut gone = 0;
        for clip in clips {
            // A five-digit stem and nothing else: the numbering is this
            // program's own, and a name from anywhere else is not a slot.
            if clip.len() != 5 || !clip.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let path = smartcut_core::bdav::stream_of(&at, &clip);
            // A slot that was never written to is not an error -- the run
            // may have been stopped before it reached that recording.
            match std::fs::remove_file(&path) {
                Ok(()) => gone += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
        Ok(gone)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Wrap the finished disc in an image a burner can take.
///
/// The folder is what was written and it stays unless the run asks for it to
/// go ([`bdav_drop`]): the image is made *of* it, and deleting somebody's
/// disc because they asked for an image of it is not this program's decision
/// to make on its own. The image goes beside the folder, under the same name
/// -- `disc/` becomes `disc.iso`.
#[tauri::command]
async fn bdav_image(
    app: tauri::AppHandle,
    dir: String,
    title: String,
    revision: String,
    access: String,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let at = local_path(&dir)?;
        let revision = smartcut_core::udfw::Revision::parse(&revision)
            .ok_or_else(|| format!("{revision}: not a UDF revision this writes"))?;
        // What the burned disc is to say about itself. A window that has not
        // been asked -- an older one, or one where the row was never drawn --
        // gets what a burned disc is.
        let access = smartcut_core::udfw::Access::parse(&access).unwrap_or_default();
        // Appended rather than `with_extension`, which would take a folder
        // called `2026.09` and write `2026.iso`.
        let image = std::path::PathBuf::from(format!("{}.iso", at.display()));
        let reporter = app.clone();
        smartcut_core::udfw::write(
            &at,
            &image,
            revision,
            access,
            &title,
            Some(&move |done: f64| {
                let _ = reporter.emit("image-progress", done);
            }),
        )
        .map_err(|e| e.to_string())?;
        Ok(image.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Take the written disc away, now that the image holds all of it.
///
/// Asked for separately from the image and only ever after one was written:
/// the image is made *of* the folder, so an image that failed leaves the
/// disc exactly where it is, and a removal that fails does not take the
/// image down with it. What goes is `BDAV` and, where that leaves it empty,
/// the folder above -- a disc written straight into a folder of somebody's
/// own leaves what else is in it alone. See
/// [`smartcut_core::bdav::remove_disc`].
#[tauri::command]
async fn bdav_drop(dir: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let at = local_path(&dir)?;
        smartcut_core::bdav::remove_disc(&at).map_err(|e| e.to_string())?;
        Ok(at.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Write the index around the streams the run has just written.
///
/// This is the pass that makes a folder of `00001.m2ts` into a disc: each
/// stream is read back for its arrival times and its entry points, and the
/// playlists and the clip indexes are written around them. Minutes on a
/// disc's worth of material, so it says how far along it is.
#[tauri::command]
async fn bdav_finish(
    app: tauri::AppHandle,
    dir: String,
    title: String,
    entries: Vec<BdavEntry>,
) -> Result<u64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let at = local_path(&dir)?;
        let recordings: Vec<smartcut_core::bdav::Recording> = entries
            .into_iter()
            .map(|e| smartcut_core::bdav::Recording {
                clip: e.clip,
                name: e.name,
                made: e.made.as_deref().and_then(smartcut_core::si::Began::parse),
                ran: e.ran,
                description: e.description,
                channel: e.channel,
                channel_number: e.channel_number.unwrap_or(0),
                marks: e.marks,
            })
            .collect();
        let reporter = app.clone();
        smartcut_core::bdav::write(
            &at,
            &title,
            &recordings,
            Some(&move |clip: &str, done: f64| {
                let _ = reporter.emit("bdav-progress", (clip.to_string(), done));
            }),
        )
        .map_err(|e| e.to_string())?;
        // What the disc actually came to, now that everything on it is
        // written. The screen said what it expected before the run; this is
        // the answer, and the two are not always the same -- a recording whose
        // pictures would not shrink as far as they were asked to is a disc
        // larger than the one that was drawn. See `fit`.
        Ok(folder_bytes(&at))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// How much a folder holds, everything under it counted.
fn folder_bytes(at: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(at) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => folder_bytes(&e.path()),
            _ => e.metadata().map(|m| m.len()).unwrap_or(0),
        })
        .sum()
}

/// The stretches asked for, with anything the cut will not cover taken out.
///
/// `keeps` is the edit itself and `asked` the stretches to play, which are
/// the same list except when ループ is on and one window of the timeline is
/// wanted. The edit is planned and the windows clipped to what came back.
///
/// **A range's bounds are the window's own numbers, and the plan can move
/// one.** A bound that does not sit on a picture, and sits less than a
/// picture before an entry point, leaves no room for the head the cut would
/// have re-encoded -- so the output begins at the entry point instead.
///
/// A cut point put there by hand is a picture time and is never moved. A
/// Trim line read off a sidecar is another matter: it counts frames, and
/// the window turns a frame number into a time on an even grid, `head + n /
/// fps`. **Not every broadcast is evenly spaced.** Measured over two minutes
/// of two recordings: one steps 33.37 ms from first picture to last and the
/// grid lands on every one of them, to a thousandth of a millisecond. The
/// other carries 47 pictures with a field repeated -- 50 ms where the rest
/// are 33.37 -- and one of those is enough to put everything after it half a
/// frame off the grid: 3509 of its 3557 pictures, with 237 of the grid's
/// points landing inside the window that moves a bound.
///
/// Planned through `build_plan`, which is the door the editor's own panel
/// and the export both come through: three answers about one edit that
/// could disagree would be three answers too many.
fn to_play(src: &Source, keeps: &[(f64, f64)], asked: &[(f64, f64)]) -> Vec<(f64, f64)> {
    if keeps.is_empty() {
        return asked.to_vec();
    }
    let covered: Vec<(f64, f64)> = build_plan(src, keeps)
        .iter()
        .map(|p| (p.t_in, p.t_out))
        .collect();
    clipped(&covered, asked)
}

/// What is left of `asked` once everything outside `covered` is taken out.
fn clipped(covered: &[(f64, f64)], asked: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for (a, b) in asked {
        for (t_in, t_out) in covered {
            let (lo, hi) = (a.max(*t_in), b.min(*t_out));
            if hi > lo + 1e-9 {
                out.push((lo, hi));
            }
        }
    }
    out
}

/// Play the edited timeline back from `from`, as a stream of pictures.
///
/// Paced against a wall clock on the *edited* timeline, so a cut costs no
/// time; pictures whose moment has already gone are dropped rather than
/// shown late, which keeps playback in time instead of letting it run slow.
#[tauri::command]
// The arguments are the command's interface, as they are on `export`: what
// the window sends is what the engine plays.
#[allow(clippy::too_many_arguments)]
async fn play(
    app: tauri::AppHandle,
    ranges: Vec<(f64, f64)>,
    keeps: Vec<(f64, f64)>,
    from: f64,
    width: u32,
    fps: f64,
    run: u64,
    frames: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>,
) -> Result<(), String> {
    // The window this plays for may have gone already: see [`EditorUp`].
    if !app.state::<EditorUp>().0.load(Ordering::SeqCst) {
        return Ok(());
    }
    // The pictures come from the proxy when there is one; the sound always
    // comes from the recording, which is where the audio actually is.
    let src = with_pictures(&app, |s, _| Ok(s.clone()))?;
    let audio_from = {
        let state = app.state::<Opened>();
        let guard = locked(&state.0);
        guard.as_ref().ok_or("no file open")?.clone()
    };
    app.state::<Playing>().0.store(run, Ordering::SeqCst);
    // Both halves play the same stretches, so this is settled before either
    // of them starts. See [`to_play`].
    let ranges = to_play(&audio_from, &keeps, &ranges);

    // Audio runs on its own thread and its own clock (see `play_audio`'s
    // doc comment): it just keeps a ring buffer fed, and the sound card
    // paces itself. `Playing` is the one thing the two sides share, so
    // stopping either one stops both.
    // Nothing has been heard of this run yet, and what the meter holds is
    // the end of the last one.
    app.state::<Meter>().0.clear();
    let audio_handle = audio_from.audio.is_some().then(|| {
        let level = app.state::<Vol>().0.clone();
        let meter = app.state::<Meter>().0.clone();
        let audio_src = audio_from.clone();
        let audio_ranges = ranges.clone();
        let audio_app = app.clone();
        std::thread::spawn(move || {
            let stop_app = audio_app.clone();
            // Either answer stops it: this run is no longer the one in force
            // -- stopped, or another started -- or the window it plays for
            // has gone. The second is what covers the moment between the two
            // -- a close that lands while this is starting up leaves
            // `Playing` set, and only the window can still say otherwise.
            let stop = move || {
                stop_app.state::<Playing>().0.load(Ordering::SeqCst) != run
                    || !stop_app.state::<EditorUp>().0.load(Ordering::SeqCst)
            };
            if let Err(e) =
                smartcut_core::play_audio(&audio_src, &audio_ranges, from, &level, &meter, stop)
            {
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
        let up = app.state::<EditorUp>();
        let began = std::time::Instant::now();
        let mut elapsed_out = 0.0f64;
        let mut next_show = 0.0f64;
        let mut outcome: Result<(), String> = Ok(());

        for (a, b) in ranges {
            if playing.0.load(Ordering::SeqCst) != run || !up.0.load(Ordering::SeqCst) {
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
                    if playing.0.load(Ordering::SeqCst) != run || !up.0.load(Ordering::SeqCst) {
                        return smartcut_core::Pace::Stop;
                    }
                    let out_t = (base + t - seg_from).max(0.0);
                    let due = std::time::Duration::from_secs_f64(out_t);
                    let now = began.elapsed();
                    if due > now {
                        std::thread::sleep(due - now);
                    } else if out_t + 2.0 * gap < now.as_secs_f64() {
                        // Already more than two pictures late. Showing it
                        // would put the picture further behind the sound
                        // rather than catch it up -- the sound plays at the
                        // card's own speed and waits for nothing -- so it
                        // goes by instead. A skip costs the decode and no
                        // more, which is what lets the window ask for the
                        // recording's full rate on a machine that cannot
                        // encode that many: the ones it can encode are still
                        // shown at the moment they are due.
                        return smartcut_core::Pace::Skip;
                    }
                    // And no more often than the rate asked for, which is the
                    // recording's own: each picture shown is a JPEG to encode
                    // and a data URL for the webview to take apart, and a
                    // second picture inside one frame's worth of time is one
                    // nobody sees. Everything between costs a decode and no
                    // more.
                    //
                    // Half a frame of slack, not a hair's breadth. At the
                    // recording's own rate every picture lands on the moment
                    // this last asked for, and the two numbers are arrived at
                    // differently -- one by adding `gap` up, the other read
                    // off a timestamp -- so they agree to a rounding error
                    // rather than exactly. Judged on that error, every other
                    // picture would fall on the wrong side of it and be
                    // dropped.
                    if out_t + gap / 2.0 < next_show {
                        return smartcut_core::Pace::Skip;
                    }
                    next_show = out_t + gap;
                    smartcut_core::Pace::Show
                },
                // Down a channel rather than out as an event, and as the
                // JPEG's own bytes rather than as a data URL. An event is
                // JSON, so a picture has to be base64 first: at 1280 wide
                // that is 99 KB of JPEG written out as 132 KB of text,
                // thirty times a second -- 4 MB/s of string for the window
                // to parse and decode back. A channel hands a large payload
                // over as bytes, which is a quarter less to carry and
                // nothing to encode at either end.
                //
                // The instant goes in front of the picture rather than
                // beside it, because one message is one message: two would
                // be two things to keep in step, and the large ones are
                // fetched by the window in their own time and can arrive
                // out of order. Eight bytes, little endian, then the JPEG.
                |t, jpeg| {
                    let mut body = Vec::with_capacity(8 + jpeg.len());
                    body.extend_from_slice(&t.to_le_bytes());
                    body.extend_from_slice(&jpeg);
                    let _ = frames.send(tauri::ipc::InvokeResponseBody::Raw(body));
                },
            );
            if let Err(e) = r {
                outcome = Err(e.to_string());
                break;
            }
            elapsed_out += b - start;
        }

        // Only if this run is still the one in force: a newer one has its own
        // name in there, and clearing it would stop the playback that has
        // just begun.
        let _ = playing
            .0
            .compare_exchange(run, 0, Ordering::SeqCst, Ordering::SeqCst);
        if let Some(h) = audio_handle {
            let _ = h.join();
        }
        // Named, so that the window can tell the end of the run it is showing
        // from the end of one it has already moved on from.
        let _ = app.emit("play-ended", run);
        outcome
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What one subtitle track has on screen at an instant, for the preview to
/// draw over the picture.
///
/// Two shapes, because the recordings have two. A broadcast's captions are
/// characters and where to put them: the window draws them itself, with the
/// fonts it has, which is why they stay sharp however big the stage is. A
/// disc's subtitles are a picture, and it arrives as a PNG cropped to
/// itself, to be placed on the same screen the positions are measured
/// against.
#[derive(Serialize)]
struct Overlay {
    /// The screen everything below is placed on: the caption plane for a
    /// broadcast, the picture's own size for a disc.
    width: u16,
    height: u16,
    runs: Vec<TextRun>,
    picture: Option<Picture>,
}

#[derive(Serialize)]
struct TextRun {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    /// How far apart the characters are, which is what lets the window
    /// place each one where the broadcaster put it rather than where a font
    /// would.
    advance: u16,
    text: String,
    /// The dots of one character, where the broadcaster sent the picture of
    /// it rather than a code for it. A run carrying this is that character
    /// and nothing else, and its `text` is empty.
    glyph: Option<Glyph>,
    /// `#rrggbb`.
    colour: String,
}

/// A character a broadcaster drew: an arrow carrying a sentence onto the
/// next line, the brackets around a speaker's name. See
/// `smartcut_core::caption::Glyph`.
#[derive(Serialize)]
struct Glyph {
    /// The dots across and down, which is the character cell it fills.
    width: u16,
    height: u16,
    /// One bit a dot, rows in order, each row starting on a byte and the
    /// high bit of a byte the leftmost dot -- base64, because this goes
    /// through JSON on its way to the window.
    ink: String,
}

#[derive(Serialize)]
struct Picture {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    url: String,
}

/// The subtitle on screen at `time`, or nothing where there is none.
///
/// `id` is the track, by the number the track chooser lists it under. A
/// different one throws the last reader away and opens another, which is
/// what switching languages amounts to.
#[tauri::command]
async fn subtitle_at(
    id: i32,
    time: f64,
    app: tauri::AppHandle,
) -> Result<Option<Overlay>, String> {
    off_thread(move || {
        let state = app.state::<Subs>();
        let mut guard = locked(&state.0);
        if guard.as_ref().is_none_or(|s| s.id != id) {
            let opened = app.state::<Opened>();
            let src = locked(&opened.0);
            let src = src.as_ref().ok_or("no file open")?;
            *guard = Some(Subtitles {
                id,
                reader: smartcut_core::subs::Reader::open(src, id).map_err(|e| e.to_string())?,
            });
        }
        let subs = guard.as_mut().expect("just built");
        let shown = subs.reader.at(time).map_err(|e| e.to_string())?;
        Ok(shown.map(|shown| match shown {
            smartcut_core::subs::Shown::Text { plane, runs } => Overlay {
                width: plane.0,
                height: plane.1,
                runs: runs
                    .iter()
                    .map(|r| TextRun {
                        x: r.x,
                        y: r.y,
                        width: r.width,
                        height: r.height,
                        advance: r.advance,
                        text: r.text.clone(),
                        glyph: r.glyph.as_ref().map(|g| Glyph {
                            width: u16::from(g.width),
                            height: u16::from(g.height),
                            ink: base64::engine::general_purpose::STANDARD.encode(&g.ink),
                        }),
                        colour: format!("#{:06x}", r.colour),
                    })
                    .collect(),
                picture: None,
            },
            smartcut_core::subs::Shown::Picture {
                screen,
                x,
                y,
                width,
                height,
                png,
            } => Overlay {
                width: screen.0,
                height: screen.1,
                runs: Vec::new(),
                picture: Some(Picture {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: *height,
                    url: format!(
                        "data:image/png;base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(png)
                    ),
                }),
            },
        }))
    })
    .await
}

#[tauri::command]
fn stop_play(playing: State<Playing>) {
    playing.0.store(0, std::sync::atomic::Ordering::SeqCst);
}

/// How loud to play, as a multiplier on the samples: 0 for silence, 1 for
/// the recording as it is. Takes effect at once when playback is running,
/// and stands for the next one when it is not.
///
/// What a slider's position means in loudness is settled in the window that
/// draws the slider -- see `gain` there -- so this takes the answer rather
/// than a percentage.
#[tauri::command]
fn set_volume(level: f64, volume: State<Vol>) {
    volume.0.set(level as f32);
}

/// What the sound has reached since this was last asked, per channel.
///
/// Emptied by the asking: a meter draws the peak of the moment it is
/// drawing, not a total since playback began. The window asks about twenty
/// times a second while something is playing and stops asking when it stops.
///
/// Cheap on purpose -- a few atomic reads, no lock and no decoding -- because
/// it is on a timer. Silence comes back as an empty list, which is what a
/// window that is not playing anything gets.
#[tauri::command]
fn audio_levels(meter: State<Meter>) -> Vec<f32> {
    meter.0.take()
}

/// What the sound reaches around `time`, for a meter with nothing playing.
///
/// The other half of the same readout. Stepping a frame at a time past the
/// end of a programme is exactly when somebody wants to see the level, and
/// [`audio_levels`] has nothing to say there: no sound is being played, so
/// none is being metered.
///
/// One frame's worth of sound, so the bars answer for the picture on the
/// stage rather than for a stretch around it. Read from the recording -- a
/// proxy carries no sound to read.
#[tauri::command]
async fn audio_peak_at(time: f64, window: f64, app: tauri::AppHandle) -> Result<Vec<f32>, String> {
    off_thread(move || {
        // Cloned, and the lock let go before the read: this runs while the
        // window is otherwise idle, and a detection can be holding the
        // recording for minutes. See [`opened_clone`].
        let src = {
            let state = app.state::<Opened>();
            let guard = locked(&state.0);
            guard.as_ref().ok_or("no file open")?.clone()
        };
        smartcut_core::peaks_at(&src, time, window).map_err(|e| e.to_string())
    })
    .await
}

/// Write the keyframe list beside the output.
///
/// One frame number per line, CRLF, and nothing else -- the shape the tools
/// that read these files expect. Numbered against the file just written, not
/// the recording it came from.
#[tauri::command]
async fn write_keyframes(path: String, frames: Vec<u32>, fps: f64) -> Result<usize, String> {
    off_thread(move || write_keyframes_now(&path, &frames, fps)).await
}

fn write_keyframes_now(path: &str, frames: &[u32], fps: f64) -> Result<usize, String> {
    use std::fmt::Write as _;
    let _ = fps;
    let mut body = String::new();
    for f in frames {
        let _ = write!(body, "{f}\r\n");
    }
    std::fs::write(path, body).map_err(|e| e.to_string())?;
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
async fn read_keyframes(path: String) -> Result<Option<Vec<u32>>, String> {
    off_thread(move || read_keyframes_now(&path)).await
}

fn read_keyframes_now(path: &str) -> Result<Option<Vec<u32>>, String> {
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    Ok(Some(
        body.lines().filter_map(|l| l.trim().parse::<u32>().ok()).collect(),
    ))
}

/// Write a mark list this side does not know the shape of.
///
/// The `.keyframe` above is written here because it is a list of numbers and
/// nothing else. An AviSynth `Trim` line is a sentence in another program's
/// language, and only the window holding the timeline can write it: what
/// survives a cut is up there. So this takes the text whole, the way
/// [`write_project`] does, and owns only the disc.
#[tauri::command]
async fn write_sidecar(path: String, body: String) -> Result<(), String> {
    off_thread(move || {
        std::fs::write(&path, body)
            .map_err(|e| trf!("保存できません: {} ({})", "Cannot save: {} ({})", path, e))
    })
    .await
}

/// Read one back, or `None` when there is none.
///
/// Missing is the ordinary case rather than an error, the same as
/// [`read_keyframes`]: these files are looked for beside a recording on the
/// off-chance that somebody left one there. It is also how "is there already
/// one?" gets asked, before writing over it.
#[tauri::command]
async fn read_sidecar(path: String) -> Result<Option<String>, String> {
    off_thread(move || match std::fs::read_to_string(&path) {
        Ok(body) => Ok(Some(body)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    })
    .await
}

/// Write a project file.
///
/// The list window builds the text: a project is the rows, what has been cut
/// out of each of them and where the results are to go, and all of that is up
/// there. Same division as [`write_keyframes`] -- whoever holds the state
/// owns the shape of the file, and this side owns the disc.
#[tauri::command]
async fn write_project(path: String, body: String) -> Result<(), String> {
    off_thread(move || {
        std::fs::write(&path, body)
            .map_err(|e| trf!("保存できません: {} ({})", "Cannot save: {} ({})", path, e))
    })
    .await
}

/// Read one back.
///
/// A missing file is an error here, unlike the keyframe sidecar's: that one
/// is looked for on the off-chance, and this one was named by the user out of
/// a file picker and is expected to be there.
#[tauri::command]
async fn read_project(path: String) -> Result<String, String> {
    off_thread(move || {
        std::fs::read_to_string(&path)
            .map_err(|e| trf!("開けません: {} ({})", "Cannot open: {} ({})", path, e))
    })
    .await
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

/// 終了 on the menu: ask this window to close, exactly as its cross does.
///
/// Not [`quit`], which is the answer to the question rather than the asking
/// of it. A menu item that went straight there would be the one way out of
/// the program that could take an evening's cuts with it.
#[tauri::command]
fn close_main(app: tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN) {
        let _ = w.close();
    }
}

// --- バッチ出力 -----------------------------------------------------------
//
// The queue of projects to write out, and the second process that writes
// them. Both halves are here because both are about a file two programs
// share.
//
// **The batch tool is a process of its own.** It is this same executable
// started with `--batch`: the window it opens is the same window, showing the
// queue and the output screen and nothing else, so a job runs through exactly
// the code a person runs it through by hand. Its own process because that is
// what the thing is for -- a queue lined up at midnight has to go on running
// when the window it was lined up in is closed.
//
// Which leaves the queue itself, which both processes read and one of them
// writes. A file, then, rather than the window's own store: a job added in
// the main window while the tool is running has to reach the tool, and the
// tool's progress has to reach the window. Re-read on a timer by both -- two
// seconds, which is a queue -- and written whole by whichever of them changed
// it, with the tool's changes winning while it runs because the main window's
// side of it is then append-only. See `batch_append`.

/// One job: the project to write out, and how it went.
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
struct BatchJob {
    path: String,
    label: String,
    /// waiting, running, done, error or skipped. A string because it is the
    /// frontend's word for the row's class as much as it is a state.
    state: String,
    note: String,
    /// When the project behind this row was last written over, in
    /// milliseconds, or 0 for one nobody has edited since it was queued.
    ///
    /// A number the tool compares against rather than reads: what a row shows
    /// about a project -- how many recordings it holds, the picture off the
    /// first of them -- is read once and kept, and this is how the window
    /// that edited the file tells the tool that what it kept is stale. See
    /// [`batch_touch`].
    edited: u64,
}

/// The queue as it sits on disk.
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct BatchQueue {
    jobs: Vec<BatchJob>,
    /// What to do once it is empty: nothing, sleep or shutdown.
    after: String,
}

/// Spelled out rather than derived, so that a queue written by a window that
/// was only adding to it still says what it would do when it empties. The
/// frontend reads an empty string as this anyway; a file somebody opens
/// should not need to know that.
impl Default for BatchQueue {
    fn default() -> Self {
        BatchQueue {
            jobs: Vec::new(),
            after: "nothing".to_string(),
        }
    }
}

/// Where the queue and the tool's heartbeat live: beside the preferences
/// rather than in the caches, because neither is something a person clearing
/// their scratch files means to throw away.
fn batch_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| {
        format!(
            "{}: {e}",
            tr!("設定の置き場が分かりません", "No config directory")
        )
    })?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Where the queue keeps the projects it was handed rather than pointed at.
///
/// バッチに登録 writes the list on screen to a file, a job being a file. A
/// list that has no file of its own gets one here rather than in the output
/// folder: it is a copy the queue asked for, not work somebody saved, and a
/// `.scproj` nobody asked for sitting next to the recordings is litter. It is
/// the queue's to delete, which is what [`drop_temp_project`] is for.
fn queue_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = batch_dir(app)?.join("queued");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// A name nothing in `dir` has yet: `stem.ext`, or the first of `stem-2.ext`,
/// `stem-3.ext` … that is free.
///
/// Worked out on this side because this is the side that can see the folder:
/// a name the list window merely believed to be free would be a registration
/// that wrote over the project it was handed last night. The extension comes
/// from up there with the rest of what a project is.
fn free_in(dir: &std::path::Path, stem: &str, ext: &str) -> Result<String, String> {
    for n in 1..1000 {
        let name = if n == 1 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem}-{n}.{ext}")
        };
        let at = dir.join(name);
        if !at.exists() {
            return Ok(at.to_string_lossy().into_owned());
        }
    }
    Err(trf!(
        "名前が付けられません: {}",
        "Cannot find a free name: {}",
        dir.join(format!("{stem}.{ext}")).display()
    ))
}

/// Where to write a list that is going into the queue. The name is the list
/// window's; the folder is this side's.
#[tauri::command]
fn queue_temp_path(app: tauri::AppHandle, stem: String, ext: String) -> Result<String, String> {
    free_in(&queue_dir(&app)?, &stem, &ext)
}

/// Take a copy of a project that is already on disc, for ジョブ追加.
///
/// The queue owns every project it runs, however the project reached it. A
/// row pointing at somebody's own file is a row that changes when they edit
/// it and breaks when they move it, and a queue that deleted such a file with
/// the row would be deleting their work; a copy answers all three. What they
/// picked is left exactly as it was.
#[tauri::command]
async fn queue_copy(app: tauri::AppHandle, path: String) -> Result<String, String> {
    off_thread(move || queue_copy_now(&app, &path)).await
}

fn queue_copy_now(app: &tauri::AppHandle, path: &str) -> Result<String, String> {
    let from = std::path::Path::new(path);
    let stem = from
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".into());
    let ext = from
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "scproj".into());
    let at = free_in(&queue_dir(app)?, &stem, &ext)?;
    std::fs::copy(from, &at)
        .map_err(|e| trf!("開けません: {} ({})", "Cannot open: {} ({})", path, e))?;
    Ok(at)
}

/// Throw away a project the queue wrote for itself, as the job that named it
/// leaves the queue.
///
/// Only ever a file in the folder above, whoever asks: the queue is handed
/// paths, and a queue that deleted whatever path it was handed would be one
/// row's removal away from deleting somebody's work. A project that was
/// saved and then queued is not the queue's to throw away, and a path outside
/// the folder is answered `false` and left alone -- as is one already gone,
/// which is the answer for every row of a queue written before this existed.
#[tauri::command]
fn drop_temp_project(app: tauri::AppHandle, path: String) -> Result<bool, String> {
    let dir = queue_dir(&app)?;
    let at = std::path::Path::new(&path);
    if at.parent() != Some(dir.as_path()) || !at.is_file() {
        return Ok(false);
    }
    std::fs::remove_file(at).map_err(|e| e.to_string())?;
    Ok(true)
}

/// The queue, or an empty one where there has never been a job.
///
/// A file that will not parse is an empty queue as well. It is a list of work
/// to do rather than the work itself, and a tool that refused to start
/// because of it would be a tool nobody could clear.
#[tauri::command]
fn batch_read(app: tauri::AppHandle) -> BatchQueue {
    let Ok(dir) = batch_dir(&app) else {
        return BatchQueue::default();
    };
    std::fs::read_to_string(dir.join("batch.json"))
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())
        .unwrap_or_default()
}

/// Put the queue down whole.
///
/// Written beside itself and renamed over, so that a process reading it while
/// this runs reads one state or the other and never half of each.
#[tauri::command]
fn batch_write(app: tauri::AppHandle, queue: BatchQueue) -> Result<(), String> {
    let dir = batch_dir(&app)?;
    let body = serde_json::to_string_pretty(&queue).map_err(|e| e.to_string())?;
    let temp = dir.join("batch.json.new");
    std::fs::write(&temp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, dir.join("batch.json")).map_err(|e| e.to_string())
}

/// Add to the end of the queue without touching the rest of it.
///
/// What the main window uses, and the reason the two processes do not have to
/// take turns: the tool owns every other field of every other row, so a job
/// handed over while it is running lands behind the one it is working on and
/// nothing it has written is read back stale and put down again.
///
/// A path already in the queue is not added twice.
#[tauri::command]
fn batch_append(app: tauri::AppHandle, jobs: Vec<BatchJob>) -> Result<BatchQueue, String> {
    let mut queue = batch_read(app.clone());
    for job in jobs {
        if queue.jobs.iter().any(|j| j.path == job.path) {
            continue;
        }
        queue.jobs.push(job);
    }
    batch_write(app, queue.clone())?;
    Ok(queue)
}

/// Say that the project behind one row has been written over.
///
/// For the window the tool opens with プロジェクトを開く: it writes the file,
/// and the tool is the one holding a picture and a count read out of the
/// version before. Read-modify-write like [`batch_append`], and as safe for
/// the same reason -- what it touches is a field the tool never writes.
///
/// `note` is the same sentence [`batch_append`] wrote when the job was added,
/// said again about the list as it now stands, and it lands only on a row
/// that is still waiting: a row that has run says how it went, and how many
/// recordings the project holds is not news worth that line.
#[tauri::command]
fn batch_touch(app: tauri::AppHandle, path: String, note: Option<String>) -> Result<(), String> {
    let mut queue = batch_read(app.clone());
    let mut found = false;
    for job in queue.jobs.iter_mut() {
        if job.path == path {
            job.edited = now_millis();
            if let Some(note) = note.clone() {
                if job.state == "waiting" {
                    job.note = note;
                }
            }
            found = true;
        }
    }
    if !found {
        return Ok(());
    }
    batch_write(app, queue)
}

/// How long a tool that has stopped saying anything is still believed.
///
/// Three heartbeats. The tool says so every ten seconds, and a machine busy
/// enough to write two discs at once is a machine that can miss one.
const BATCH_BEAT: u64 = 30;

/// The same clock finer, for the one thing a second is not enough for: two
/// saves of one project inside a second have to read as two.
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The tool saying it is still here. Called on a timer by the batch window.
///
/// Its own name goes in beside the time, for [`batch_gone`]: a beat is taken
/// away by the tool that wrote it and by nobody else.
#[tauri::command]
fn batch_beat(app: tauri::AppHandle) {
    if let Ok(dir) = batch_dir(&app) {
        let beat = format!("{} {}", std::process::id(), now_secs());
        let _ = std::fs::write(dir.join("batch.beat"), beat);
    }
}

/// The tool saying it has gone. Called as its process leaves.
///
/// Without this the tool is believed for [`BATCH_BEAT`] seconds after its
/// window is closed, and somebody who closes it with the cross and opens it
/// again is told there is already one running -- which is a program that has
/// not noticed its own window close. The clock is still what decides, because
/// a tool that is killed outright never gets here; this is the fast path out
/// of it, not a replacement for it.
///
/// Only this process's own beat is taken away. A tool wedged past
/// [`BATCH_BEAT`] lets a second one start, and the first one leaving must not
/// then clear the second one's name off the door.
fn batch_gone(app: &tauri::AppHandle) {
    let Ok(dir) = batch_dir(app) else { return };
    let beat = dir.join("batch.beat");
    if beating(&beat).is_some_and(|(whose, _)| whose == Some(std::process::id())) {
        let _ = std::fs::remove_file(&beat);
    }
}

/// What the heartbeat file says: which process wrote it, and when.
///
/// `"pid seconds"`. A file written before the pid was in there reads as a
/// time with nobody's name on it, which is a beat only the clock can retire.
fn beating(path: &std::path::Path) -> Option<(Option<u32>, u64)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut said = text.split_whitespace();
    let first = said.next()?;
    match said.next() {
        Some(at) => Some((first.parse().ok(), at.parse().ok()?)),
        None => Some((None, first.parse().ok()?)),
    }
}

/// Whether a batch tool is running.
///
/// By the clock rather than by asking the operating system about a process
/// id, which is the one way to ask that means the same thing on all three
/// platforms -- and the one that cannot mistake some unrelated program that
/// has since been given the same number for a batch tool.
///
/// Asked only by [`open_batch_tool`]. The window that opens the tool does not
/// otherwise care: it opens one, and is told if there is already one.
fn batch_live(app: tauri::AppHandle) -> bool {
    let Ok(dir) = batch_dir(&app) else {
        return false;
    };
    beating(&dir.join("batch.beat"))
        .is_some_and(|(_, at)| now_secs().saturating_sub(at) < BATCH_BEAT)
}

/// Start the batch tool: this same program, with `--batch`.
///
/// One is never started over another. Two tools over one queue would each
/// believe they owned it, and the file says which jobs are done.
///
/// `false` for a tool that was already there, rather than an error: it is
/// news only where somebody asked for the tool by name. バッチに登録 also
/// wants one up, and for that press "there is one already" is the request
/// granted, not refused.
#[tauri::command]
fn open_batch_tool(app: tauri::AppHandle) -> Result<bool, String> {
    if batch_live(app) {
        return Ok(false);
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe)
        .arg("--batch")
        .spawn()
        .map(|_| true)
        .map_err(|e| e.to_string())
}

/// Put this window in the middle of the screen.
///
/// The tool asks once it is up rather than being placed as it is built: a
/// window started from another window is placed by the desktop against the
/// one that started it, and on the screen this was tested on that left it
/// half off the bottom. Asked after the page is running, which is after the
/// desktop has had its say.
#[tauri::command]
fn center_window(app: tauri::AppHandle, role: State<Role>) {
    // Unless the tool has already been put back where it was last left. The
    // desktop's guess is what this is for, and a remembered place is not a
    // guess. See [`geometry`].
    if geometry::placed(&role.0) {
        return;
    }
    if let Some(w) = app.get_webview_window(MAIN) {
        let _ = w.center();
    }
}

/// Open one of the queue's projects in a list window of its own.
///
/// The tool cannot show a project -- the two screens where a list is looked at
/// are off its bar, and the list it holds at any moment is a job it is in the
/// middle of writing. So a job somebody wants to look at is handed to a fresh
/// window: this same program, started on that file, which is what the command
/// line and a file manager already do with a `.scproj`.
#[tauri::command]
fn open_project_window(path: String, queued: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut run = std::process::Command::new(exe);
    run.arg(path);
    // Which is the whole of the difference: the same list window, told that
    // the project it is opening is a job somebody has queued. What it does
    // about that is [`queued_job`].
    if queued {
        run.arg(QUEUED_FLAG);
    }
    run.spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// How a window is told that the project on its command line is a queued job.
/// A flag rather than a second path, because the path is already there --
/// and because [`Argv`] keeps only the arguments that are not flags.
const QUEUED_FLAG: &str = "--queued";

/// The queued job this window was opened on, or `None` for a window that is
/// nobody's job.
///
/// The list window asks once, at startup. What it does with the answer is
/// offer バッチを上書き where it would otherwise offer バッチに登録: a window
/// opened out of the queue is there to put something back into it.
#[tauri::command]
fn queued_job(argv: State<Argv>, queued: State<Queued>) -> Option<String> {
    if !queued.0 {
        return None;
    }
    argv.0.first().cloned()
}

/// Whether this window was started on a queued job. See [`QUEUED_FLAG`].
struct Queued(bool);

/// Show a folder in whatever the desktop uses to show folders.
///
/// For the one question a finished queue leaves: where did it put them. The
/// platform's own opener rather than anything of this program's -- a list of
/// files is a file manager's business, and every desktop already has the one
/// its owner chose.
#[tauri::command]
fn show_folder(path: String) -> Result<(), String> {
    let dir = std::path::Path::new(&path);
    if !dir.is_dir() {
        return Err(trf!(
            "フォルダーが見つかりません: {}",
            "No such folder: {}",
            path
        ));
    }
    let opener = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{opener}: {e}"))
}

/// Which of the two this window is: the list window, or the batch tool.
///
/// The frontend is the same page either way -- the tool needs the list and
/// the output screen to do the work -- so this is what it asks to know which
/// of its screens to put on the bar.
#[tauri::command]
fn window_role(role: State<Role>) -> String {
    role.0.clone()
}

struct Role(String);

/// Put the machine to sleep, or turn it off, now that the queue is empty.
///
/// The one thing a batch tool is for that a list of jobs is not: somebody who
/// queues six discs at midnight is not sitting there at four. The frontend
/// asks for this only after the countdown on the batch screen has run out
/// with nobody stopping it, so by the time it arrives here the answer has
/// been given twice.
///
/// Handed to the platform's own command rather than done here. Each of these
/// is what the machine's own menu would have run, and the ones that need to
/// be root say so themselves rather than being run as root by this: a
/// program that cuts video has no business holding that.
#[tauri::command]
fn after_batch(what: String) -> Result<(), String> {
    let sleep = what == "sleep";
    let (program, args): (&str, &[&str]) = if cfg!(target_os = "windows") {
        match sleep {
            // The documented way to suspend from a command line, and the
            // only one: Windows has no `shutdown` switch for sleep. The
            // three numbers are hibernate-no, force-yes, wake-events-no.
            true => ("rundll32.exe", &["powrprof.dll,SetSuspendState", "0,1,0"]),
            false => ("shutdown", &["/s", "/t", "0"]),
        }
    } else if cfg!(target_os = "macos") {
        match sleep {
            true => ("pmset", &["sleepnow"]),
            false => (
                "osascript",
                &["-e", "tell application \"System Events\" to shut down"],
            ),
        }
    } else {
        match sleep {
            true => ("systemctl", &["suspend"]),
            false => ("systemctl", &["poweroff"]),
        }
    };
    std::process::Command::new(program)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{program}: {e}"))
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

// --- 環境設定 -------------------------------------------------------------
//
// The frontend holds the preferences, for the reason it holds the language:
// it is the one with somewhere to keep them. What is here is the half this
// side acts on -- see [`prefs`] -- plus the two questions only this side can
// answer, which are where the scratch files are and how much of them there
// is.

/// The four as the panel sends them.
///
/// Every field defaults, so a window from a build that had one fewer of them
/// still settles the rest instead of failing the whole call.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct PrefsIn {
    clean_joins: bool,
    proxy: bool,
    proxy_width: u32,
    ffmpeg_log: u8,
    /// Empty for the platform's own place.
    cache_dir: String,
    /// Seconds. `0` for none, which is the default.
    audio_fade: f64,
}

/// What is in force, for the panel to paint itself from.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct PrefsOut {
    clean_joins: bool,
    proxy: bool,
    proxy_width: u32,
    ffmpeg_log: u8,
    cache_dir: String,
    audio_fade: f64,
    /// Where they would go if nobody chose, so that 既定 is a place with a
    /// name on screen rather than an empty field.
    cache_home: String,
}

/// Settle them.
///
/// The folder is the only one that can be refused: the other three are a
/// flag or a small number and mean something whatever they are set to, while
/// a folder that does not exist or cannot be written to would be found out
/// at the next open, in the middle of a pass, with nobody looking at the
/// panel that asked for it. So it is made and written to here, and a refusal
/// comes back as a sentence the panel can show.
#[tauri::command]
fn set_prefs(want: PrefsIn) -> Result<(), String> {
    let asked = match want.cache_dir.trim() {
        "" => Ok(None),
        chosen => usable_cache_dir(chosen).map(Some),
    };
    // The other three are settled either way. A folder that has gone missing
    // since it was chosen -- an external disk, a share not mounted yet -- is
    // no reason to leave a window running with the joins and the proxy set to
    // whatever they were before: the scratch files go back to the place the
    // platform gives, which is where they were before anybody chose, and the
    // refusal is reported.
    let dir = asked.as_ref().ok().and_then(|d| d.clone());
    prefs::set(
        want.clean_joins,
        want.proxy,
        want.proxy_width,
        want.ffmpeg_log,
        dir,
        want.audio_fade,
    );
    asked.map(|_| ())
}

/// The folder, if it can hold anything.
///
/// Made if it is not there yet: what a picker hands back is a place, not a
/// place already in use. And then written to, because made is not the same as
/// writable -- a folder on a share that has gone read-only is already there.
fn usable_cache_dir(chosen: &str) -> Result<std::path::PathBuf, String> {
    let dir = std::path::PathBuf::from(chosen);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let probe = dir.join(".smartcut-write-test");
    std::fs::write(&probe, b"").map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&probe);
    Ok(dir)
}

/// What is in force now, which at startup is what the environment said.
#[tauri::command]
fn prefs_now(app: tauri::AppHandle) -> PrefsOut {
    PrefsOut {
        clean_joins: prefs::clean_join().is_some(),
        proxy: prefs::proxy(),
        proxy_width: prefs::proxy_width().unwrap_or(0),
        ffmpeg_log: prefs::ffmpeg_log(),
        audio_fade: prefs::audio_fade(),
        cache_dir: prefs::cache_dir().map(|d| d.display().to_string()).unwrap_or_default(),
        cache_home: app
            .path()
            .app_cache_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_default(),
    }
}

/// How much one kind of scratch file is taking up.
#[derive(Serialize, Default)]
struct CacheUse {
    files: u64,
    bytes: u64,
}

/// The three kinds, and where they are.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct CacheReport {
    dir: String,
    index: CacheUse,
    proxy: CacheUse,
    cm: CacheUse,
    /// The two flat detections together, which is how they are kept: one
    /// folder holding a `.blkj` and a `.qtj` per recording. Told apart
    /// nowhere the panel can see, and there is nothing to tell apart --
    /// whichever of them is there was written by a pass over this recording
    /// and would be made again the same way.
    flat: CacheUse,
}

/// Every kind of work kept in the cache, which is what the panel lists and
/// what すべて削除 empties. Named once, so that a pass which starts writing
/// somewhere new is one edit away from being counted and cleared with the
/// rest rather than growing on the disc unmentioned.
const CACHE_KINDS: [&str; 4] = ["index", "proxy", "cm", "flat"];

/// What one folder holds. A folder that is not there holds nothing, which is
/// the answer rather than an error: none of them is made until the first
/// thing goes into it.
fn folder_use(dir: &std::path::Path) -> CacheUse {
    let mut held = CacheUse::default();
    let Ok(entries) = std::fs::read_dir(dir) else { return held };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        held.files += 1;
        held.bytes += meta.len();
    }
    held
}

/// What is on disk, for the panel to show before it asks about deleting it.
///
/// By kind, because they do not cost the same to lose: an index is a pass
/// over the recording, a proxy is a whole re-encode of it, a commercial
/// detection is both, and the flat detections are a full decode. Counted
/// when asked rather than kept running -- the panel asks once per opening,
/// and four `read_dir`s is nothing beside what made the files.
#[tauri::command]
async fn cache_usage(app: tauri::AppHandle) -> Result<CacheReport, String> {
    off_thread(move || {
        let root = cache_root(&app)?;
        Ok(CacheReport {
            dir: root.display().to_string(),
            index: folder_use(&root.join("index")),
            proxy: folder_use(&root.join("proxy")),
            cm: folder_use(&root.join("cm")),
            flat: folder_use(&root.join("flat")),
        })
    })
    .await
}

/// Delete them.
///
/// The files inside the cache folders, and not the folders: what is being
/// thrown away is what a pass would build again, and a folder somebody chose
/// in 環境設定 is not that. Nothing here can lose any of the user's work --
/// the cuts are in the clip list and in the project file -- so a file that
/// will not go is passed over rather than stopped on, and what is left is
/// reported by the count the panel asks for next.
#[tauri::command]
async fn clear_cache(app: tauri::AppHandle) -> Result<(), String> {
    off_thread(move || {
        let root = cache_root(&app)?;
        for kind in CACHE_KINDS {
            let Ok(entries) = std::fs::read_dir(root.join(kind)) else { continue };
            for entry in entries.flatten() {
                if entry.metadata().map(|m| m.is_file()).unwrap_or(false) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Ok(())
    })
    .await
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
    // And the preferences the environment can also set, which is how they
    // were reachable before 環境設定 had a row for each of them. The frontend
    // sends what is actually stored as soon as it has read its own store;
    // until then these are what a pass started from the command line runs
    // with. See [`prefs`].
    prefs::from_env();

    let argv = Argv(std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect());
    // Started as the batch tool rather than as the list window. The same
    // program and the same page; what differs is which screens are on the bar
    // and that nothing about an unsaved list stands in the way of closing it.
    // See the バッチ出力 section.
    let batch = std::env::args().any(|a| a == "--batch");
    // And started on a job out of the queue, which is the list window again
    // with one button reading differently. See [`queued_job`].
    let queued = Queued(std::env::args().any(|a| a == QUEUED_FLAG));
    // The word this window's size is kept under as well as the word the
    // frontend asks for: the tool and the list are one window label and two
    // different windows to size. See [`geometry`].
    let role = match batch {
        true => "batch",
        false => "main",
    };
    tauri::Builder::default()
        // Where the editor's pictures are fetched from. See [`shot_url`].
        .register_uri_scheme_protocol(SHOT_SCHEME, |ctx, request| {
            let held = u64::from_str_radix(request.uri().path().trim_start_matches('/'), 16)
                .ok()
                .and_then(|name| {
                    let state = ctx.app_handle().state::<Shots>();
                    let store = locked(&state.0);
                    store.held.get(&name).map(|h| h.jpeg.clone())
                });
            let (status, body) = match held {
                Some(jpeg) => (200, jpeg),
                // A picture that has gone. Nothing on screen should reach
                // this: see the room [`shot_url`] keeps.
                None => (404, Vec::new()),
            };
            tauri::http::Response::builder()
                .status(status)
                .header("Content-Type", "image/jpeg")
                .header("Cache-Control", "public, max-age=31536000, immutable")
                .body(body)
                .expect("a response of bytes")
        })
        .plugin(tauri_plugin_dialog::init())
        .manage(Opened::default())
        .manage(Proxy::default())
        .manage(Generation::default())
        .manage(Thumbs::default())
        .manage(OpenPath::default())
        .manage(Held::default())
        .manage(Playing::default())
        .manage(Vol::default())
        .manage(Meter::default())
        .manage(EditorUp::default())
        .manage(CrossUp::default())
        .manage(Crossed::default())
        .manage(Subs::default())
        .manage(BatchStop::default())
        .manage(Shots::default())
        .manage(argv)
        .manage(queued)
        .manage(Role(role.to_string()))
        // The list window is declared in the configuration rather than built
        // here, so `setup` is the first moment there is one to attach
        // anything to.
        .setup(move |app| {
            // What the three windows were last left at, before there is a
            // window on screen to put it on. See [`geometry`].
            geometry::load(app.handle());
            if let Some(w) = app.get_webview_window(MAIN) {
                // The tool is named for what it is, at once: the frontend
                // retitles the list window as a project is opened, and the
                // tool opens one per job.
                if batch {
                    let _ = w.set_title(tr!("バッチ出力 — SmartCut", "Batch — SmartCut"));
                }
                // Declared invisible in the configuration, and shown here
                // with its name, its size and its place already on it.
                geometry::restore(&w, role);
                let _ = w.show();
                geometry::watch(&w, role);
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
            open_outline,
            glimpse,
            glimpses,
            glimpse_sweep,
            detect_cm,
            detect_blank,
            detect_silence,
            detect_blank_at,
            detect_quiet_at,
            flat_cached,
            thumbs_at,
            preview,
            prepare,
            hover_thumb,
            scene_search,
            make_plan,
            write_keyframes,
            read_keyframes,
            write_sidecar,
            read_sidecar,
            play,
            stop_play,
            set_volume,
            subtitle_at,
            export,
            export_joined,
            programme,
            series_title,
            bdav_prepare,
            bdav_discard,
            bdav_finish,
            disc_room,
            bdav_image,
            bdav_drop,
            audio_limits,
            containers_holding,
            index_clip,
            clip_outline,
            clip_pictures,
            detect_cm_at,
            cm_cached,
            stop_batch,
            clip_plan,
            join_fit,
            tracks,
            clip_thumbs,
            clip_poster,
            clip_glance,
            clip_gone,
            open_editor,
            open_cross,
            close_cross,
            retitle_cross,
            cross_load,
            cross_span,
            cross_shot,
            cross_play,
            editor_up,
            open_zoom,
            close_zoom,
            zoom_shot,
            audio_levels,
            audio_peak_at,
            retitle_editor,
            retitle_main,
            close_editor,
            write_project,
            read_project,
            queue_temp_path,
            queue_copy,
            drop_temp_project,
            batch_touch,
            queued_job,
            set_dirty,
            after_batch,
            batch_read,
            batch_write,
            batch_append,
            batch_beat,
            open_batch_tool,
            open_project_window,
            show_folder,
            window_role,
            free_folder,
            center_window,
            quit,
            close_main,
            set_lang,
            os_locale,
            set_prefs,
            prefs_now,
            cache_usage,
            clear_cache,
            versions
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |app, event| {
            // The one write of the sizes, on the way out: what a window is at
            // is watched all along, but dragging an edge is a hundred sizes
            // and none of them worth a file. `quit` goes through here too --
            // `exit` asks the loop to leave, it does not walk out of it.
            if let tauri::RunEvent::Exit = event {
                geometry::save(app);
                // And the tool takes its name off the door as it goes, so
                // that the list window knows at once. See `batch_gone`.
                if batch {
                    batch_gone(app);
                }
            }
        });
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

    /// What playback is given is the edit as the cut will write it, not as
    /// the window drew it. A bound the plan moved is material the output
    /// will not have, and a window of the timeline asked for by ループ has to
    /// be clipped to the same answer.
    #[test]
    fn playback_is_given_the_stretches_the_cut_covers() {
        // The plan moved the first range's start a frame later and left the
        // second alone.
        let covered = [(1.5, 3.0), (5.0, 6.0)];
        assert_eq!(
            clipped(&covered, &[(1.48, 3.0), (5.0, 6.0)]),
            vec![(1.5, 3.0), (5.0, 6.0)]
        );
        // A loop asking for one window across a join keeps its own ends and
        // takes the join where the plan put it.
        assert_eq!(clipped(&covered, &[(2.9, 5.1)]), vec![(2.9, 3.0), (5.0, 5.1)]);
        // A window wholly inside what was cut away plays nothing.
        assert!(clipped(&covered, &[(3.2, 4.8)]).is_empty());
        // And one that touches a bound by a rounding is not a stretch.
        assert!(clipped(&covered, &[(3.0, 5.0)]).is_empty());
    }

    /// 環境設定 sends a folder for the scratch files, and a folder that cannot
    /// hold them has to be refused while the panel that asked for it is still
    /// on screen -- not at the next open, in the middle of a pass. What is
    /// settled is settled as sent: a refusal leaves the previous answer in
    /// force.
    #[test]
    fn a_cache_folder_that_cannot_be_written_to_is_refused() {
        let mut root = std::env::temp_dir();
        root.push(format!("smartcut-prefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let good = root.join("scratch");

        let want = |dir: &str| PrefsIn {
            clean_joins: true,
            proxy: false,
            proxy_width: 960,
            ffmpeg_log: 0,
            cache_dir: dir.to_string(),
            audio_fade: 0.0,
        };
        // A folder that is not there yet is made rather than refused: what
        // the picker hands back is a place, not a place already in use.
        assert!(set_prefs(want(&good.display().to_string())).is_ok());
        assert_eq!(prefs::cache_dir(), Some(good));
        assert_eq!(prefs::clean_join(), Some(2.0));
        assert_eq!(prefs::proxy_width(), Some(960));

        // A file where a folder was named. `create_dir_all` fails on it, and
        // the scratch files go back to the platform's own place -- while the
        // settings sent with it are settled all the same.
        let file = root.join("not-a-folder");
        std::fs::write(&file, b"").unwrap();
        assert!(set_prefs(want(&file.display().to_string())).is_err());
        assert_eq!(prefs::cache_dir(), None);
        assert_eq!(prefs::proxy_width(), Some(960));

        // And empty means the platform's own place, which is not this side's
        // to name -- `None` is how that is said.
        assert!(set_prefs(want("")).is_ok());
        assert_eq!(prefs::cache_dir(), None);

        let _ = std::fs::remove_dir_all(&root);
    }
}
