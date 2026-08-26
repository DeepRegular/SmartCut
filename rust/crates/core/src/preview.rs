//! Single-frame extraction, for scrubbing a timeline.
//!
//! A webview cannot play MPEG-2 in a transport stream, so the picture under
//! the playhead has to be decoded here and handed over as an image.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;

use crate::{AccessPoint, Source};

/// A decoded picture, with what the cutter cares about knowing about it.
pub struct Shot {
    pub jpeg: Vec<u8>,
    /// Presentation time of the picture actually returned.
    pub time: f64,
    /// "I", "P" or "B" -- an I picture is a place a cut costs nothing.
    pub kind: &'static str,
}

fn kind_of(frame: &ff::frame::Video) -> &'static str {
    match frame.kind() {
        ff::picture::Type::I => "I",
        ff::picture::Type::P => "P",
        ff::picture::Type::B => "B",
        _ => "-",
    }
}

/// Decode pictures at exactly these times, in the order asked for.
///
/// The film strip walks the *edited* timeline, so two neighbouring cells can
/// sit either side of a cut and be minutes apart in the recording. Times that
/// run on contiguously are decoded together -- one seek pays for the lot --
/// and a jump starts a fresh run.
///
/// A slot with no picture near it comes back empty rather than shifting the
/// ones after it along: the strip keeps the playhead in its middle cell, and
/// that only holds if the cells stay where they were put.
pub fn shots_at(src: &Source, times: &[f64], width: u32) -> Result<Vec<Option<Shot>>> {
    crate::init()?;
    if times.is_empty() {
        return Ok(Vec::new());
    }
    let fd = src.video.frame_duration();

    // What counts as "a jump" has to be read off the request: cells a frame
    // apart and cells two seconds apart are both evenly spaced, and neither
    // should be split.
    let base = times
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|d| *d > 1e-9)
        .fold(f64::INFINITY, f64::min);
    let jump = if base.is_finite() { base * 2.5 + 0.5 } else { f64::INFINITY };

    let mut out: Vec<Option<Shot>> = (0..times.len()).map(|_| None).collect();
    let mut start = 0usize;
    for i in 1..=times.len() {
        let split = i == times.len() || times[i] - times[i - 1] > jump || times[i] < times[i - 1];
        if !split {
            continue;
        }
        for (k, shot) in collect_run(src, &times[start..i], width, fd)?.into_iter().enumerate() {
            out[start + k] = shot;
        }
        start = i;
    }
    Ok(out)
}

/// One seek, then a straight decode filling every slot with the nearest
/// picture to it.
fn collect_run(
    src: &Source,
    wanted: &[f64],
    width: u32,
    fd: f64,
) -> Result<Vec<Option<Shot>>> {
    let first = wanted[0];
    let last = *wanted.last().unwrap();
    let spacing = if wanted.len() > 1 {
        (last - first) / (wanted.len() - 1) as f64
    } else {
        fd
    };
    let window = spacing.max(fd);
    let from = entry_before(&src.points, first);

    let mut got: Vec<Option<(f64, &'static str, ff::frame::Video)>> = Vec::new();
    for (attempt, margin) in [0.0, src.seek_margin].into_iter().enumerate() {
        let mut slots: Vec<Option<(f64, &'static str, ff::frame::Video)>> =
            (0..wanted.len()).map(|_| None).collect();
        let began = walk(src, from, margin, |t, frame| {
            if t > last + fd {
                return false;
            }
            for (i, &w) in wanted.iter().enumerate() {
                let better = match &slots[i] {
                    Some((have, _, _)) => (t - w).abs() < (have - w).abs(),
                    None => (t - w).abs() <= window,
                };
                if better {
                    slots[i] = Some((t, kind_of(frame), frame.clone()));
                }
            }
            true
        })?;
        got = slots;
        if !landed_late(began, first, window / 2.0) || attempt == 1 {
            break;
        }
    }

    // Two slots can round to the same picture -- the strip's spacing need not
    // be a whole number of picture intervals, and under pulldown it never is.
    // The later slot gives it up rather than repeating it.
    for i in (1..got.len()).rev() {
        let dup = match (&got[i], &got[i - 1]) {
            (Some(a), Some(b)) => (a.0 - b.0).abs() < fd / 2.0,
            _ => false,
        };
        if dup {
            got[i] = None;
        }
    }

    got.into_iter()
        .map(|slot| match slot {
            Some((t, kind, f)) => {
                Ok(Some(Shot { jpeg: encode_jpeg(&f, src, width)?, time: t, kind }))
            }
            None => Ok(None),
        })
        .collect()
}

/// What to do with a picture that has just been decoded.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    /// Encode it and hand it over.
    Show,
    /// Let it go by. Costs only the decode, which is the point: dropping has
    /// to be cheaper than showing or playback can never catch up.
    Skip,
    Stop,
}

/// Decode forward from `from`, offering every picture to the caller.
///
/// For playing a stretch back rather than scrubbing it. `pace` sees each
/// picture's time and decides what to do with it -- that is where waiting and
/// dropping belong, because only the caller knows what the clock says.
pub fn play_from(
    src: &Source,
    from: f64,
    until: f64,
    width: u32,
    mut pace: impl FnMut(f64) -> Pace,
    mut show: impl FnMut(f64, Vec<u8>),
) -> Result<()> {
    crate::init()?;
    let fd = src.video.frame_duration();
    let entry = entry_before(&src.points, from);
    let mut stopped = false;
    for (attempt, margin) in [0.0, src.seek_margin].into_iter().enumerate() {
        let mut began_late = true;
        let first = walk(src, entry, margin, |t, frame| {
            if t > until + fd / 2.0 {
                stopped = true;
                return false;
            }
            if t < from - fd / 2.0 {
                return true;
            }
            began_late = false;
            match pace(t) {
                Pace::Stop => {
                    stopped = true;
                    false
                }
                Pace::Skip => true,
                Pace::Show => {
                    if let Ok(jpeg) = encode_jpeg(frame, src, width) {
                        show(t, jpeg);
                    }
                    true
                }
            }
        })?;
        // Landing past the wanted picture means nothing was played; back off
        // and start again earlier, as the scrubbing paths do.
        if !landed_late(first, from, fd / 2.0) || !began_late || attempt == 1 || stopped {
            break;
        }
    }
    Ok(())
}

/// Decode the picture shown at `time` and return it as a JPEG.
///
/// Seeks to the access point before the target rather than to the target
/// itself: an open GOP cannot be decoded from anywhere else, and broadcast
/// material is open-GOP throughout.
pub fn frame_at(src: &Source, time: f64, width: u32) -> Result<Vec<u8>> {
    shot_at(src, time, width).map(|s| s.jpeg)
}

/// As [`frame_at`], but also reporting what kind of picture it is.
pub fn shot_at(src: &Source, time: f64, width: u32) -> Result<Shot> {
    crate::init()?;
    let fd = src.video.frame_duration();
    let from = entry_before(&src.points, time);

    let mut picture: Option<(f64, ff::frame::Video)> = None;
    for (attempt, margin) in [0.0, src.seek_margin].into_iter().enumerate() {
        let mut hit: Option<(f64, ff::frame::Video)> = None;
        let mut tail: Option<(f64, ff::frame::Video)> = None;
        let began = walk(src, from, margin, |t, frame| {
            // The wanted picture is whichever of the two straddling `time` is
            // nearer -- not "the first one at or after it". Under 2:3
            // pulldown the pictures are 41.7ms apart inside a 29.97 fps
            // stream, so assuming a picture every frame duration picks the
            // wrong side of the gap.
            if t >= time - 1e-6 {
                let after = (t - time).abs();
                hit = match tail.take() {
                    Some((pt, pf)) if (time - pt).abs() < after => Some((pt, pf)),
                    _ => Some((t, frame.clone())),
                };
                return false;
            }
            tail = Some((t, frame.clone()));
            true
        })?;
        if !landed_late(began, time, fd / 2.0) || attempt == 1 {
            picture = hit.or(tail);
            break;
        }
    }

    let (at, picture) = picture.ok_or_else(|| anyhow!("no picture at {time:.3}s"))?;
    let kind = kind_of(&picture);
    Ok(Shot { jpeg: encode_jpeg(&picture, src, width)?, time: at, kind })
}

/// The latest access point at or before `time`, so the decode has references.
fn entry_before(points: &[AccessPoint], time: f64) -> f64 {
    points
        .iter()
        .rev()
        .find(|p| p.time <= time + 1e-6)
        .map(|p| p.time)
        .unwrap_or(0.0)
}

pub(crate) fn encode_jpeg(picture: &ff::frame::Video, src: &Source, width: u32) -> Result<Vec<u8>> {
    let sar = src.video.sample_aspect_ratio.max(0.01);
    let out_w = width.max(16) & !1;
    // Downscaling far enough also takes the comb out of interlaced material,
    // so a preview needs no deinterlacer of its own.
    let out_h = (((out_w as f64 * picture.height() as f64)
        / (picture.width() as f64 * sar))
        .round() as u32)
        .max(16)
        & !1;

    let mut scaler = ff::software::scaling::Context::get(
        picture.format(),
        picture.width(),
        picture.height(),
        ff::format::Pixel::YUVJ420P,
        out_w,
        out_h,
        ff::software::scaling::Flags::AREA,
    )?;
    let mut scaled = ff::frame::Video::empty();
    scaler.run(picture, &mut scaled)?;

    let codec = ff::encoder::find(ff::codec::Id::MJPEG)
        .ok_or_else(|| anyhow!("no MJPEG encoder"))?;
    let mut enc = ff::codec::context::Context::new_with_codec(codec).encoder().video()?;
    enc.set_width(out_w);
    enc.set_height(out_h);
    enc.set_format(ff::format::Pixel::YUVJ420P);
    enc.set_time_base(ff::Rational::new(1, 25));
    unsafe {
        (*enc.as_mut_ptr()).flags |= ff::ffi::AV_CODEC_FLAG_QSCALE as i32;
        (*enc.as_mut_ptr()).global_quality = ff::ffi::FF_QP2LAMBDA * 4;
    }
    let mut enc = enc.open_as(codec)?;

    scaled.set_pts(Some(0));
    enc.send_frame(&scaled)?;
    enc.send_eof()?;
    let mut packet = ff::Packet::empty();
    let mut out = Vec::new();
    while enc.receive_packet(&mut packet).is_ok() {
        out.extend_from_slice(packet.data().unwrap_or(&[]));
        packet = ff::Packet::empty();
    }
    if out.is_empty() {
        return Err(anyhow!("the JPEG encoder produced nothing"));
    }
    Ok(out)
}

/// Decode forward from an access point, handing each picture to `visit`.
///
/// Returns the presentation time of the first picture that came out, which is
/// how the caller learns that the seek landed too late. A transport stream is
/// seeked by byte position, so the landing is approximate: it can arrive past
/// the picture that was asked for, or inside a GOP whose sequence header has
/// already gone by -- and then the whole first GOP decodes to nothing. Both
/// look identical from here, and both are fixed the same way, by starting
/// again a few GOPs earlier.
///
/// `visit` returns false to stop.
fn walk(
    src: &Source,
    from: f64,
    margin: f64,
    mut visit: impl FnMut(f64, &ff::frame::Video) -> bool,
) -> Result<Option<f64>> {
    let mut ictx = ff::format::input(&src.path)?;
    let idx = src.video.stream_index;
    let in_tb = src.video.time_base;
    let landing = (from - margin).max(0.0);
    let target = ((landing + src.start_time) * ff::ffi::AV_TIME_BASE as f64) as i64;
    let _ = ictx.seek(target, ..target);

    let params =
        ictx.stream(idx).ok_or_else(|| anyhow!("stream {idx} vanished"))?.parameters();
    let mut decoder =
        ff::codec::context::Context::from_parameters(params)?.decoder().video()?;

    let mut frame = ff::frame::Video::empty();
    let mut first = None;
    let mut stopped = false;
    'outer: for (stream, packet) in ictx.packets() {
        if stream.index() != idx {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        while decoder.receive_frame(&mut frame).is_ok() {
            let Some(pts) = frame.pts() else { continue };
            let t = pts as f64 * in_tb - src.start_time;
            if first.is_none() {
                first = Some(t);
            }
            if !visit(t, &frame) {
                stopped = true;
                break 'outer;
            }
        }
    }
    if !stopped {
        // Whatever is still held for reordering, so a request at the very end
        // of the file is answered rather than falling off it.
        let _ = decoder.send_eof();
        while decoder.receive_frame(&mut frame).is_ok() {
            let t = frame.pts().map(|p| p as f64 * in_tb - src.start_time).unwrap_or(from);
            if first.is_none() {
                first = Some(t);
            }
            if !visit(t, &frame) {
                break;
            }
        }
    }
    Ok(first)
}

/// Did decoding begin late enough to have missed the picture wanted?
fn landed_late(first: Option<f64>, wanted: f64, slack: f64) -> bool {
    !matches!(first, Some(f) if f <= wanted + slack)
}
