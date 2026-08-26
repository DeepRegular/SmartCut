//! A coarse thumbnail track and a scene index, both built in one pass.
//!
//! Hovering the scrubber has to put a picture on screen as fast as the
//! pointer moves, which rules out decoding on demand -- a seek plus a GOP is
//! a few hundred milliseconds and the requests would queue up behind the
//! pointer. So the pictures are decoded once, up front, and kept.
//!
//! Scene detection wants the same pass. Comparing successive key pictures is
//! what finds a cut, and the key pictures are exactly what is being decoded
//! for the thumbnails, so the scene index costs a few hundred bytes on top of
//! work already being done.
//!
//! Only intra pictures are touched: about eight times cheaper than a full
//! decode, and broadcast material carries one every half second, which is
//! finer than either job needs.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;

use crate::Source;

pub struct ThumbOptions {
    /// Shortest spacing between kept thumbnails. Every key picture is
    /// compared, but only pictures this far apart are held as images.
    ///
    /// Zero means "keep every key picture", which is what a scroll search
    /// wants: dragging across the film strip shows the held pictures, so
    /// their spacing is the granularity of the scroll.
    pub interval: f64,
    /// Ceiling on how many are held, whatever `interval` says. A long
    /// recording widens its own spacing rather than filling memory.
    pub max_thumbs: usize,
    /// Thumbnail width, in pixels.
    pub width: u32,
    /// Picture difference below which nothing counts as a cut, however quiet
    /// the recording is. Without it, a still programme would report a scene
    /// change at every twitch.
    pub floor: f64,
    /// How far above the recording's own typical difference a cut has to
    /// stand. Read from the material because a talk show and a music video
    /// disagree by an order of magnitude about what "typical" is.
    pub over_typical: f64,
    /// Never report scenes closer together, on average, than this. A guard
    /// against material where the whole picture churns.
    pub min_spacing: f64,
}

impl Default for ThumbOptions {
    fn default() -> Self {
        Self {
            interval: 0.0,
            max_thumbs: 4000,
            width: 192,
            floor: 0.055,
            over_typical: 3.0,
            min_spacing: 3.0,
        }
    }
}

pub struct Thumb {
    pub time: f64,
    pub jpeg: Vec<u8>,
}

pub struct Track {
    pub width: u32,
    /// The spacing actually used, which is what a caller must assume when
    /// deciding whether the held pictures are fine enough for its purpose.
    pub interval: f64,
    /// Held images, in time order.
    pub thumbs: Vec<Thumb>,
    /// Times of the key pictures that open a new scene, in time order.
    pub scenes: Vec<f64>,
    /// The difference that had to be exceeded to make that list, and the
    /// recording's typical difference -- both reported so the choice can be
    /// judged rather than trusted.
    pub threshold: f64,
    pub typical: f64,
}

impl Track {
    /// The held picture nearest `time`, for a hover preview.
    pub fn nearest(&self, time: f64) -> Option<&Thumb> {
        if self.thumbs.is_empty() {
            return None;
        }
        let i = self.thumbs.partition_point(|t| t.time < time);
        let cand = [i.wrapping_sub(1), i];
        cand.iter()
            .filter_map(|&j| self.thumbs.get(j))
            .min_by(|a, b| {
                (a.time - time).abs().total_cmp(&(b.time - time).abs())
            })
    }

    pub fn scene_after(&self, time: f64) -> Option<f64> {
        self.scenes.iter().copied().find(|&s| s > time + 1e-3)
    }

    pub fn scene_before(&self, time: f64) -> Option<f64> {
        self.scenes.iter().rev().copied().find(|&s| s < time - 1e-3)
    }
}

/// Width and height of the picture signature, in cells.
const SIG_W: usize = 16;
const SIG_H: usize = 9;
const SIG: usize = SIG_W * SIG_H;

/// Reduce a picture to a grid of block averages of its luma.
///
/// Coarse on purpose: a scene change moves whole regions of the picture, and
/// anything finer would answer camera shake as loudly.
fn signature(frame: &ff::frame::Video) -> [u8; SIG] {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let stride = frame.stride(0);
    let data = frame.data(0);
    let mut out = [0u8; SIG];
    if w == 0 || h == 0 || data.len() < (h - 1) * stride + w {
        return out;
    }
    for cy in 0..SIG_H {
        let y0 = cy * h / SIG_H;
        let y1 = (((cy + 1) * h) / SIG_H).max(y0 + 1);
        for cx in 0..SIG_W {
            let x0 = cx * w / SIG_W;
            let x1 = (((cx + 1) * w) / SIG_W).max(x0 + 1);
            let (mut sum, mut n) = (0u32, 0u32);
            // Sampling every fourth pixel: the block mean is all that is
            // wanted and reading them all costs sixteen times as much.
            let mut y = y0;
            while y < y1 {
                let row = y * stride;
                let mut x = x0;
                while x < x1 {
                    sum += data[row + x] as u32;
                    n += 1;
                    x += 4;
                }
                y += 4;
            }
            out[cy * SIG_W + cx] = (sum / n.max(1)) as u8;
        }
    }
    out
}

fn distance(a: &[u8; SIG], b: &[u8; SIG]) -> f64 {
    let sum: u32 = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as u32).sum();
    sum as f64 / (SIG as f64 * 255.0)
}

/// Decode every key picture once: keep some as images, compare them all.
pub fn build(
    src: &Source,
    opts: &ThumbOptions,
    mut progress: Option<Box<dyn FnMut(f64) + Send>>,
) -> Result<Track> {
    crate::init()?;
    let mut ictx = ff::format::input(&src.path)?;
    let idx = src.video.stream_index;
    let params = ictx.stream(idx).ok_or_else(|| anyhow!("video stream vanished"))?.parameters();
    let mut decoder = ff::codec::context::Context::from_parameters(params)?.decoder().video()?;

    let interval =
        opts.interval.max(src.duration / opts.max_thumbs.max(1) as f64);
    let mut thumbs: Vec<Thumb> = Vec::new();
    let mut diffs: Vec<(f64, f64)> = Vec::new();
    let mut prev: Option<[u8; SIG]> = None;
    let mut keep_from = f64::NEG_INFINITY;
    let mut told = -1.0;
    let mut frame = ff::frame::Video::empty();

    let mut take = |frame: &ff::frame::Video| -> Result<()> {
        let Some(pts) = frame.pts() else { return Ok(()) };
        let t = pts as f64 * src.video.time_base - src.start_time;
        if t < -1.0 {
            return Ok(());
        }
        let sig = signature(frame);
        if let Some(p) = &prev {
            diffs.push((t, distance(p, &sig)));
        }
        prev = Some(sig);

        if t >= keep_from {
            keep_from = t + interval;
            thumbs.push(Thumb {
                time: t,
                jpeg: crate::preview::encode_jpeg(frame, src, opts.width)?,
            });
        }
        if let Some(f) = progress.as_mut() {
            let done = (t / src.duration.max(1e-9)).clamp(0.0, 1.0);
            if done - told >= 0.01 {
                told = done;
                f(done);
            }
        }
        Ok(())
    };

    for (stream, packet) in ictx.packets() {
        if stream.index() != idx {
            continue;
        }
        // Non-key packets are not merely discarded but never handed over:
        // the decoder would still have to parse them to throw them away.
        //
        // Skipping is done here rather than with `skip_frame`, which would be
        // wrong: that switch is per *picture*, and an interlaced access unit
        // is not always one picture. Broadcast MPEG-2 switches between frame
        // and field coding as it goes, and a field-coded entry point is an I
        // top field followed by a *P* bottom field -- one packet, two
        // pictures. AVDISCARD_NONKEY throws the bottom field's slices away
        // for being P, leaving half a picture decoded or none at all. On a
        // BS Fuji recording that lost 351 of 3371 entry points in runs tens
        // of seconds long, and the film strip filled the holes with whatever
        // picture happened to be nearest.
        if !packet.is_key() {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        while decoder.receive_frame(&mut frame).is_ok() {
            take(&frame)?;
        }
    }
    // The decoder holds a picture back for reordering, so the last entry
    // point of the file only comes out on a flush. Without it the strip's
    // final cell has a hole in it.
    let _ = decoder.send_eof();
    while decoder.receive_frame(&mut frame).is_ok() {
        take(&frame)?;
    }
    if let Some(f) = progress.as_mut() {
        f(1.0);
    }

    let (scenes, threshold, typical) = mark_scenes(&diffs, src.duration, opts);
    Ok(Track { width: opts.width, interval, thumbs, scenes, threshold, typical })
}

/// Turn the per-key-picture differences into a list of scene starts.
fn mark_scenes(diffs: &[(f64, f64)], duration: f64, opts: &ThumbOptions) -> (Vec<f64>, f64, f64) {
    if diffs.is_empty() {
        return (Vec::new(), opts.floor, 0.0);
    }
    let mut sorted: Vec<f64> = diffs.iter().map(|d| d.1).collect();
    sorted.sort_by(f64::total_cmp);
    let typical = sorted[sorted.len() / 2];

    let mut threshold = (typical * opts.over_typical).max(opts.floor);
    // Material where everything moves would otherwise report a cut every
    // half second; raise the bar until the marks are at least plausibly far
    // apart, taking the threshold straight off the distribution.
    let cap = (duration / opts.min_spacing).max(1.0) as usize;
    let over = |th: f64| sorted.iter().filter(|&&d| d >= th).count();
    if over(threshold) > cap {
        threshold = sorted[sorted.len().saturating_sub(cap).min(sorted.len() - 1)];
    }
    if std::env::var_os("SMARTCUT_DEBUG_SCENES").is_some() {
        let q = |p: f64| sorted[((sorted.len() - 1) as f64 * p) as usize];
        eprintln!(
            "diffs n={} p50={:.4} p75={:.4} p90={:.4} p95={:.4} p99={:.4} max={:.4}",
            sorted.len(),
            q(0.50),
            q(0.75),
            q(0.90),
            q(0.95),
            q(0.99),
            q(1.0)
        );
    }
    let scenes = diffs.iter().filter(|d| d.1 >= threshold).map(|d| d.0).collect();
    (scenes, threshold, typical)
}

/// The picture a cut happens on, nearest `at`.
///
/// Not "the nearest mark in the scene index": that index is built from key
/// pictures half a second apart, and a commercial has cuts of its own, so the
/// nearest mark to an estimated boundary is as likely to be one of those as
/// the one at the edge. This decodes the window itself and takes the largest
/// change between consecutive pictures, which is the cut -- and answers with
/// `at` unchanged when there is no cut in the window worth the name.
pub fn cut_near(src: &Source, at: f64, window: f64, floor: f64) -> Result<f64> {
    crate::init()?;
    let fd = src.video.frame_duration();
    let from = (at - window).max(0.0);
    let entry = src
        .points
        .iter()
        .rev()
        .find(|p| p.time <= from + 1e-9)
        .map(|p| p.time)
        .unwrap_or(0.0);
    let until = at + window;

    let mut ictx = ff::format::input(&src.path)?;
    let idx = src.video.stream_index;
    // A transport stream seeks by byte position and can land a GOP late.
    let landing = (entry - src.seek_margin).max(0.0);
    let target = ((landing + src.start_time) * ff::ffi::AV_TIME_BASE as f64) as i64;
    let _ = ictx.seek(target, ..target);
    let params = ictx.stream(idx).ok_or_else(|| anyhow!("video stream vanished"))?.parameters();
    let mut decoder = ff::codec::context::Context::from_parameters(params)?.decoder().video()?;

    let mut seen: Vec<(f64, [u8; SIG])> = Vec::new();
    let mut frame = ff::frame::Video::empty();
    'outer: for (stream, packet) in ictx.packets() {
        if stream.index() != idx {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        while decoder.receive_frame(&mut frame).is_ok() {
            let Some(pts) = frame.pts() else { continue };
            let t = pts as f64 * src.video.time_base - src.start_time;
            if t > until + fd {
                break 'outer;
            }
            if t >= from - fd {
                seen.push((t, signature(&frame)));
            }
        }
    }
    seen.sort_by(|a, b| a.0.total_cmp(&b.0));
    // The *nearest* cut, not the strongest. A commercial cuts between images
    // as unlike each other as anything at its edges, so "the biggest change
    // in the window" reaches past the boundary and lands inside the break --
    // measured at a second's reach, where it moved a boundary a full second
    // the wrong way. The estimate coming in is already close; all this has to
    // do is put it on the picture the change happens on.
    let mut best: Option<(f64, f64)> = None;
    let show = std::env::var_os("SMARTCUT_DEBUG_CUT").is_some();
    for w in seen.windows(2) {
        let d = distance(&w[0].1, &w[1].1);
        if show && d >= floor / 3.0 {
            eprintln!("    {:9.3}  差 {:.3}{}", w[1].0, d, if d >= floor { "" } else { "  (床未満)" });
        }
        if d < floor {
            continue;
        }
        let away = (w[1].0 - at).abs();
        if best.is_none_or(|(t, _)| away < (t - at).abs()) {
            best = Some((w[1].0, d));
        }
    }
    Ok(best.map(|(t, _)| t).unwrap_or(at))
}

/// Find the exact picture a scene starts on, given the key picture that
/// first showed it.
///
/// The key pictures are half a second apart, so the change is somewhere in
/// the GOP before the one that reported it. Decoding that GOP in full -- a
/// dozen or so pictures -- puts the boundary on the right frame.
pub fn refine(src: &Source, at: f64) -> Result<f64> {
    crate::init()?;
    let fd = src.video.frame_duration();
    let from = src
        .points
        .iter()
        .rev()
        .find(|p| p.time < at - fd / 2.0)
        .map(|p| p.time)
        .unwrap_or(0.0);
    if from >= at - fd / 2.0 {
        return Ok(at);
    }

    let mut ictx = ff::format::input(&src.path)?;
    let idx = src.video.stream_index;
    // A transport stream seeks by byte position and can land a GOP late, so
    // this starts well before the picture wanted and reads forward.
    let landing = (from - src.seek_margin).max(0.0);
    let target = ((landing + src.start_time) * ff::ffi::AV_TIME_BASE as f64) as i64;
    let _ = ictx.seek(target, ..target);
    let params = ictx.stream(idx).ok_or_else(|| anyhow!("video stream vanished"))?.parameters();
    let mut decoder = ff::codec::context::Context::from_parameters(params)?.decoder().video()?;

    let mut seen: Vec<(f64, [u8; SIG])> = Vec::new();
    let mut frame = ff::frame::Video::empty();
    'outer: for (stream, packet) in ictx.packets() {
        if stream.index() != idx {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        while decoder.receive_frame(&mut frame).is_ok() {
            let Some(pts) = frame.pts() else { continue };
            let t = pts as f64 * src.video.time_base - src.start_time;
            if t > at + fd / 2.0 {
                break 'outer;
            }
            if t >= from - fd / 2.0 {
                seen.push((t, signature(&frame)));
            }
        }
    }
    seen.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut best = (at, -1.0);
    for w in seen.windows(2) {
        let d = distance(&w[0].1, &w[1].1);
        if d > best.1 {
            best = (w[1].0, d);
        }
    }
    Ok(best.0)
}
