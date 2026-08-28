//! Desktop front end for the smart-rendering cutter.
//!
//! The engine does the work; this layer holds one opened source, answers the
//! timeline's questions about it, and runs an export off the UI thread.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use base64::Engine as _;
use serde::Serialize;
use smartcut_core::{index, plan, proxy, PlanOptions, Source};
use tauri::{Emitter, Manager, State};

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

/// Reading a recording's index is a pass over the file, so it runs on a
/// worker thread; see [`off_thread`].
fn open_now(path: &str, app: &tauri::AppHandle) -> Result<SourceInfo, String> {
    let src = smartcut_core::scan(path).map_err(|e| e.to_string())?;
    // Anything still building for the file that was open belongs to nothing
    // now; the count going up is what tells it so.
    app.state::<Generation>().0.fetch_add(1, Ordering::SeqCst);
    *app.state::<Thumbs>().0.lock().unwrap() = None;
    *app.state::<Proxy>().0.lock().unwrap() = None;
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
struct PrepareInfo {
    proxy: Option<ProxyInfo>,
    track: TrackInfo,
    /// Why there is no proxy, when there is none. Empty otherwise: the
    /// recording is still perfectly editable without one, only slower to
    /// look at, so a failure here is worth saying and not worth stopping for.
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

/// Build the proxy for the open recording -- or pick up the one built last
/// time -- and the thumbnail track that comes with it.
///
/// The two are made together because they are the same pass: the pictures the
/// track is built from are the recording's access points, and those are
/// exactly the pictures the proxy is being given keyframes at. Building the
/// proxy therefore costs the thumbnails nothing, and a cached proxy means the
/// track is rebuilt from a small file instead of from the recording.
#[tauri::command]
async fn prepare(app: tauri::AppHandle) -> Result<PrepareInfo, String> {
    let (src, generation) = {
        let state = app.state::<Opened>();
        let guard = state.0.lock().unwrap();
        let src = guard.as_ref().ok_or("no file open")?.clone();
        (src, app.state::<Generation>().0.load(Ordering::SeqCst))
    };
    tauri::async_runtime::spawn_blocking(move || {
        let opts = proxy::ProxyOptions::default();
        let thumb_opts = smartcut_core::ThumbOptions::default();
        match make_proxy(&app, &src, &opts, &thumb_opts, generation) {
            Ok(Some(info)) => Ok(info),
            // Superseded: another file was opened while this ran.
            Ok(None) => Err("cancelled".to_string()),
            Err(note) => {
                // A build that was abandoned because another file was opened
                // is not a failure to fall back from: the recording it was
                // for is not the one on screen any more.
                if app.state::<Generation>().0.load(Ordering::SeqCst) != generation {
                    return Err("cancelled".to_string());
                }
                // No proxy, so the recording answers for its own pictures --
                // which is what it did before there were proxies at all. The
                // thumbnails still have to be built, from the recording.
                eprintln!("proxy: {note}");
                let began = std::time::Instant::now();
                let reporter = app.clone();
                let track = smartcut_core::thumbs::build(
                    &src,
                    &thumb_opts,
                    Some(Box::new(move |f| {
                        let _ = reporter.emit("prepare-progress", ("サムネイル", f));
                    })),
                )
                .map_err(|e| e.to_string())?;
                let info = track_info(&track, began.elapsed().as_secs_f64());
                if app.state::<Generation>().0.load(Ordering::SeqCst) != generation {
                    return Err("cancelled".to_string());
                }
                *app.state::<Thumbs>().0.lock().unwrap() = Some(track);
                Ok(PrepareInfo { proxy: None, track: info, note })
            }
        }
    })
    .await
    .map_err(|e| e.to_string())?
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
    // A way out for anyone who would rather not have a hundred megabytes per
    // recording sitting in the cache: without a proxy the recording answers
    // for its own pictures, which is how this worked before.
    if matches!(
        std::env::var("SMARTCUT_PROXY").as_deref(),
        Ok("0") | Ok("off") | Ok("no")
    ) {
        return Err("SMARTCUT_PROXY で無効にされています".to_string());
    }
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("キャッシュの置き場が分かりません: {e}"))?
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
                let _ = reporter.emit("prepare-progress", ("プロキシ", f));
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
    *app.state::<Thumbs>().0.lock().unwrap() = Some(track);
    *app.state::<Proxy>().0.lock().unwrap() = Some(Proxied { src: psrc, marks });
    Ok(Some(PrepareInfo { proxy: Some(info), track: tinfo, note: String::new() }))
}

/// Hold on to thumbnails a build has just produced, so that everything which
/// reads held pictures can start using them now rather than when the build
/// ends.
///
/// A build over a half-hour recording runs for a minute or two, and until it
/// finished there was nothing to answer the film strip, the scroll search or
/// the mark cards with but the recording itself -- a seek and a GOP each
/// time, with the encoder already on every core. These are the same pictures
/// that pass is decoding anyway, and it decodes them in order from the start,
/// so the part of the recording already gone past answers instantly while the
/// rest of it is still being read.
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
            let _ = reporter.emit("prepare-progress", ("サムネイル", f));
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
    let plans = build_plan(src, ranges);
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
        // Taken before the recording's lock and kept for the whole pass:
        // refining a boundary is a picture comparison and nothing more, so it
        // reads from the proxy where there is one.
        let pictures = with_pictures(&app, |s, _| Ok(s.clone()))?;
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
        smartcut_core::cm_refine_boundaries(&pictures, &mut blocks, 0.5, 0.08);

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
        .manage(Proxy::default())
        .manage(Generation::default())
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
            prepare,
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
