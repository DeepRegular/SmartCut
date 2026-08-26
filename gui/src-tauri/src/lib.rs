//! Desktop front end for the smart-rendering cutter.
//!
//! The engine does the work; this layer holds one opened source, answers the
//! timeline's questions about it, and runs an export off the UI thread.

use std::sync::Mutex;

use base64::Engine as _;
use serde::Serialize;
use smartcut_core::{index, plan, PlanOptions, Source};
use tauri::{Emitter, Manager, State};

#[derive(Default)]
struct Opened(Mutex<Option<Source>>);

/// The thumbnail track and scene index, once the background pass has built
/// them. Kept apart from [`Opened`] so that a pass lasting tens of seconds
/// never holds the lock the timeline needs to answer a keystroke.
#[derive(Default)]
struct Thumbs(Mutex<Option<smartcut_core::Track>>);

/// Set while playback is running; cleared to ask it to stop.
#[derive(Default)]
struct Playing(std::sync::atomic::AtomicBool);

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
    index_name: String,
    /// Presentation times of every random access point: the places a cut
    /// costs nothing.
    points: Vec<f64>,
    /// How many of those cannot start a copy because their leading pictures
    /// are referenced.
    unusable_points: usize,
}

#[derive(Serialize)]
struct BlockInfo {
    start: f64,
    end: f64,
    junctions: usize,
    score: f64,
}

#[derive(Serialize)]
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

/// A path given on the command line, so a file can be opened without the
/// picker -- handy when the app is launched from a file manager.
///
/// Captured before Tauri starts rather than read on demand: nothing should
/// depend on what the process's argv looks like once a webview is up.
struct Argv(Option<String>);

#[tauri::command]
fn initial_path(argv: State<Argv>) -> Option<String> {
    argv.0.clone()
}

/// Frontend diagnostics, surfaced in the process log. A webview on a
/// headless box has no console anyone can open.
#[tauri::command]
fn log(msg: String) {
    eprintln!("[js] {msg}");
}

#[tauri::command]
fn open_source(
    path: String,
    state: State<Opened>,
    thumbs: State<Thumbs>,
) -> Result<SourceInfo, String> {
    let src = smartcut_core::scan(&path).map_err(|e| e.to_string())?;
    *thumbs.0.lock().unwrap() = None;
    let info = SourceInfo {
        path: src.path.clone(),
        codec: src.video.codec.clone(),
        width: src.video.width,
        height: src.video.height,
        fps: src.video.frame_rate,
        duration: src.duration,
        interlaced: src.video.interlaced(),
        pulldown: src.video.pulldown,
        has_audio: src.audio.is_some(),
        index_name: src.index_name.to_string(),
        points: src.points.iter().map(|p| p.time).collect(),
        unusable_points: src.points.iter().filter(|p| p.open_gop() && !p.droppable).count(),
    };
    *state.0.lock().unwrap() = Some(src);
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
fn preview(time: f64, width: u32, state: State<Opened>) -> Result<Shot, String> {
    let guard = state.0.lock().unwrap();
    let src = guard.as_ref().ok_or("no file open")?;
    let s = smartcut_core::shot_at(src, time, width).map_err(|e| e.to_string())?;
    Ok(Shot { url: as_url(&s.jpeg), time: s.time, kind: s.kind.to_string() })
}

/// Pictures for the film strip, at exactly the times asked for.
///
/// The strip walks the *edited* timeline, so neighbouring cells can sit
/// either side of a cut. Asking by time rather than by "centre and spacing"
/// is what lets the caller hand over a run that jumps.
#[tauri::command]
fn thumbs_at(
    times: Vec<f64>,
    width: u32,
    exact: Option<bool>,
    state: State<Opened>,
    thumbs: State<Thumbs>,
) -> Result<Vec<Option<Shot>>, String> {
    if times.is_empty() {
        return Ok(Vec::new());
    }
    let guard = state.0.lock().unwrap();
    let src = guard.as_ref().ok_or("no file open")?;

    // The smallest gap asked for says whether the held pictures are fine
    // enough to answer with: they sit one key picture apart. `exact` overrides
    // that -- a caller asking about a cut's join needs the picture *at* the
    // time it named, because the nearest held one may be the last picture the
    // cut took away.
    let gap = times
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .filter(|d| *d > 1e-9)
        .fold(f64::INFINITY, f64::min);
    // A held picture sits *on* a key picture, so the one nearest a GOP start
    // should be that GOP's own. Further off than the track's own spacing means
    // there is a hole in it -- an entry point that arrived damaged, say -- and
    // answering with whatever is nearest would caption one picture with
    // another picture's time. Those slots are left for a real decode below.
    let held: Option<Vec<Option<Shot>>> = {
        let guard = thumbs.0.lock().unwrap();
        guard
            .as_ref()
            .filter(|t| !exact.unwrap_or(false) && gap >= t.interval * 0.9)
            .map(|track| {
                let tol = track.interval.max(src.video.frame_duration());
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
                    kind: s.kind.to_string(),
                });
            }
        }
        return Ok(out);
    }

    let shots = smartcut_core::shots_at(src, &times, width).map_err(|e| e.to_string())?;
    Ok(shots
        .into_iter()
        .map(|o| o.map(|s| Shot { url: as_url(&s.jpeg), time: s.time, kind: s.kind.to_string() }))
        .collect())
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

/// Decode the key pictures once, so that hovering the scrubber and searching
/// for the next scene are both instant afterwards.
#[tauri::command]
async fn warm_thumbs(app: tauri::AppHandle) -> Result<TrackInfo, String> {
    let src = {
        let state = app.state::<Opened>();
        let guard = state.0.lock().unwrap();
        guard.as_ref().ok_or("no file open")?.clone()
    };
    let reporter = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let began = std::time::Instant::now();
        let track = smartcut_core::thumbs::build(
            &src,
            &smartcut_core::ThumbOptions::default(),
            Some(Box::new(move |f| {
                let _ = reporter.emit("thumbs-progress", f);
            })),
        )
        .map_err(|e| e.to_string())?;
        let info = TrackInfo {
            thumbs: track.thumbs.len(),
            interval: track.interval,
            scenes: track.scenes.clone(),
            threshold: track.threshold,
            typical: track.typical,
            seconds: began.elapsed().as_secs_f64(),
        };
        *app.state::<Thumbs>().0.lock().unwrap() = Some(track);
        Ok(info)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The held picture nearest `time`. Returns nothing rather than decoding on
/// demand: this answers a pointer moving across the scrubber, and a decode
/// would take longer than the pointer stays anywhere.
#[tauri::command]
fn hover_thumb(time: f64, thumbs: State<Thumbs>) -> Option<Shot> {
    let guard = thumbs.0.lock().unwrap();
    let t = guard.as_ref()?.nearest(time)?;
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
fn scene_search(from: f64, dir: i32, app: tauri::AppHandle) -> Result<Option<f64>, String> {
    let thumbs = app.state::<Thumbs>();
    let opened = app.state::<Opened>();
    let guard = opened.0.lock().unwrap();
    let src = guard.as_ref().ok_or("no file open")?;
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
}

fn build_plan(src: &Source, ranges: &[(f64, f64)]) -> Vec<smartcut_core::RangePlan> {
    plan(&src.video, src.duration, &src.points, ranges, &PlanOptions::default())
}

#[tauri::command]
fn make_plan(ranges: Vec<(f64, f64)>, state: State<Opened>) -> Result<PlanInfo, String> {
    let mut guard = state.0.lock().unwrap();
    let src = guard.as_mut().ok_or("no file open")?;
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
    let plans = build_plan(src, &ranges);
    let copied: f64 = plans.iter().map(|p| p.copied()).sum();
    let reencoded: f64 = plans.iter().map(|p| p.reencoded()).sum();
    Ok(PlanInfo {
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
    })
}

/// Look for commercial breaks: caption resets where the broadcaster sends
/// them, otherwise runs of short silences spaced on a 15-second grid.
#[tauri::command]
async fn detect_cm(app: tauri::AppHandle, use_logo: bool) -> Result<CmResult, String> {
    // Reads the whole audio track, so it belongs off the UI thread.
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<Opened>();
        let guard = state.0.lock().unwrap();
        let src = guard.as_ref().ok_or("no file open")?;
        let opts = smartcut_core::DetectOptions::default();

        // Reading the audio is a few seconds; the logo is two passes over the
        // video and takes ten times as long. Weighting them that way is what
        // makes the bar move at an honest rate rather than sitting at 10%.
        let say = |app: &tauri::AppHandle, phase: &str, done: f64| {
            let _ = app.emit("cm-progress", (phase.to_string(), done));
        };
        // The caption stream goes first: where the broadcaster resets the
        // service at its junctions, those marks are exact and cost one pass
        // over a stream nothing has to decode. It is also the only signal of
        // the three that is cheap enough to try speculatively.
        const CAPTION_SHARE: f64 = 0.15;
        let reporter = app.clone();
        let resets = smartcut_core::caption::resets_with(
            src,
            Some(Box::new(move |f| {
                let _ = reporter
                    .emit("cm-progress", ("字幕を調べています".to_string(), f * CAPTION_SHARE));
            })),
        )
        .ok();

        // With the resets in hand neither of the other two reads anything:
        // the audio is a few seconds, but the logo is two passes over the
        // video, and it is the weaker signal wherever the marks exist.
        let rest = 1.0 - CAPTION_SHARE;
        let audio_share = if use_logo { 0.1 } else { 1.0 } * rest;
        let silences = match &resets {
            Some(_) => Vec::new(),
            None => {
                let reporter = app.clone();
                smartcut_core::find_silences_with(
                    src,
                    &opts,
                    Some(Box::new(move |f| {
                        let _ = reporter.emit(
                            "cm-progress",
                            ("音声を調べています".to_string(), CAPTION_SHARE + f * audio_share),
                        );
                    })),
                )
                .map_err(|e| e.to_string())?
            }
        };
        let cands = smartcut_core::cm_candidates(&silences, &opts);

        // The logo is the better read on how far a break runs, but not every
        // broadcaster shows one; when it is missing, the silences stand alone.
        let logo = if use_logo && resets.is_none() {
            let reporter = app.clone();
            smartcut_core::logo::detect_with(
                src,
                &Default::default(),
                Some(Box::new(move |f| {
                    let _ = reporter.emit(
                        "cm-progress",
                        (
                            "ロゴを探しています".to_string(),
                            CAPTION_SHARE + audio_share + f * (rest - audio_share),
                        ),
                    );
                })),
            )
            .ok()
        } else {
            None
        };
        say(&app, "まとめています", 1.0);
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
        smartcut_core::cm_refine_boundaries(src, &mut blocks, 0.5, 0.08);

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
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn export(
    app: tauri::AppHandle,
    ranges: Vec<(f64, f64)>,
    output: String,
    // Both switchable from the CLI; the window offers neither, so both are
    // optional here and default to the plain copy.
    audio_reencode: Option<bool>,
    audio_es: Option<bool>,
) -> Result<(), String> {
    // Cutting is minutes of I/O on a broadcast recording; keeping it off the
    // UI thread is what lets the progress bar move at all.
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<Opened>();
        let guard = state.0.lock().unwrap();
        let src = guard.as_ref().ok_or("no file open")?;
        let plans = build_plan(src, &ranges);
        let reporter = app.clone();
        let opts = smartcut_core::CutOptions {
            audio_mode: if audio_reencode.unwrap_or(false) {
                smartcut_core::AudioMode::Reencode
            } else {
                smartcut_core::AudioMode::Copy
            },
            ..Default::default()
        };
        smartcut_core::cut_with_progress(
            src,
            &plans,
            &output,
            &opts,
            Some(Box::new(move |f| {
                let _ = reporter.emit("export-progress", f);
            })),
        )
        .map_err(|e| e.to_string())?;

        // Read back out of the file just written, so what sits beside the
        // video is by construction the audio that is in it.
        if audio_es.unwrap_or(false) {
            let beside = std::path::Path::new(&output).with_extension("aac");
            smartcut_core::write_audio_es(&output, &beside.to_string_lossy())
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
    use std::sync::atomic::Ordering;
    let src = {
        let state = app.state::<Opened>();
        let guard = state.0.lock().unwrap();
        guard.as_ref().ok_or("no file open")?.clone()
    };
    app.state::<Playing>().0.store(true, Ordering::SeqCst);

    // Audio runs on its own thread and its own clock (see `play_audio`'s
    // doc comment): it just keeps a ring buffer fed, and the sound card
    // paces itself. `Playing` is the one thing the two sides share, so
    // stopping either one stops both.
    let audio_handle = src.audio.is_some().then(|| {
        let audio_src = src.clone();
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

    let argv = Argv(std::env::args().nth(1).filter(|a| !a.starts_with('-')));
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Opened::default())
        .manage(Thumbs::default())
        .manage(Playing::default())
        .manage(argv)
        .invoke_handler(tauri::generate_handler![
            log,
            initial_path,
            open_source,
            detect_cm,
            thumbs_at,
            preview,
            warm_thumbs,
            hover_thumb,
            scene_search,
            make_plan,
            write_keyframes,
            play,
            stop_play,
            export
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
