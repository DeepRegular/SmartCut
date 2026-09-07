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

pub(crate) fn kind_of(frame: &ff::frame::Video) -> &'static str {
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
    // ...with a ceiling over the top of it, because a run is decoded straight
    // through: cells a minute apart are evenly spaced too, and walking from
    // one to the next would decode the whole minute between them to throw all
    // but one picture of it away. Past a second or two the seek is cheaper
    // than the pictures it skips -- broadcast material carries an access
    // point every half second, and a seek lands on one. The film strip at its
    // wider settings asks for exactly that, and used to wait tens of seconds
    // for it.
    const WALK: f64 = 2.0;
    let jump = if base.is_finite() { (base * 2.5 + 0.5).min(WALK) } else { f64::INFINITY };

    // Every cell of a GOP-divided film strip stands on an entry point, and
    // when they all do, everything between them can go by unparsed: an entry
    // picture decodes on its own. That is the difference between decoding the
    // six seconds a run covers and decoding the thirteen pictures wanted out
    // of it, which is what the strip costs while a proxy is being built and
    // there is nothing faster to read from.
    let keys = times.iter().all(|&t| on_point(&src.points, t, fd / 2.0));

    let mut out: Vec<Option<Shot>> = (0..times.len()).map(|_| None).collect();
    let mut start = 0usize;
    for i in 1..=times.len() {
        let split = i == times.len() || times[i] - times[i - 1] > jump || times[i] < times[i - 1];
        if !split {
            continue;
        }
        for (k, shot) in
            collect_run(src, &times[start..i], width, fd, keys)?.into_iter().enumerate()
        {
            out[start + k] = shot;
        }
        start = i;
    }
    Ok(out)
}

/// Is `t` a random access point, to within `tol`?
fn on_point(points: &[AccessPoint], t: f64, tol: f64) -> bool {
    let i = points.partition_point(|p| p.time < t - tol);
    points.get(i).is_some_and(|p| (p.time - t).abs() <= tol)
}

/// One seek, then a straight decode filling every slot with the nearest
/// picture to it.
fn collect_run(
    src: &Source,
    wanted: &[f64],
    width: u32,
    fd: f64,
    keys: bool,
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
        let began = walk(src, from, margin, keys, |t, frame| {
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
                Ok(Some(Shot {
                    jpeg: encode_jpeg(&f, src.video.sample_aspect_ratio, width)?,
                    time: t,
                    kind,
                }))
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
        let first = walk(src, entry, margin, false, |t, frame| {
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
                    if let Ok(jpeg) = encode_jpeg(frame, src.video.sample_aspect_ratio, width) {
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
        let began = walk(src, from, margin, false, |t, frame| {
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
    Ok(Shot {
        jpeg: encode_jpeg(&picture, src.video.sample_aspect_ratio, width)?,
        time: at,
        kind,
    })
}

/// One picture out of a recording nothing has been read of yet.
///
/// Everything else here works off a [`Source`], which is a pass over the
/// whole file: the packets are walked to find the entry points, and only
/// then can a picture be asked for by time. That is the honest way to reach
/// an *exact* frame, and it is why the clip list has nothing to show for a
/// row until the pass over it has finished -- a minute of blank rows for a
/// folder dropped on the window, and the queued ones stay blank until their
/// turn comes.
///
/// A poster is not an exact frame. It stands for the recording, and any
/// picture from around the right part of it will do. So this asks
/// libavformat for its own approximate seek -- `into` of the way through by
/// the container's clock -- and takes the first picture that decodes after
/// it, which costs one seek and one GOP rather than a pass. What comes back
/// may be a second or two off, and on a transport stream it usually is: the
/// seek is by byte position there and lands near the instant asked for
/// rather than on it.
///
/// The picture the index pass eventually produces replaces this one, and
/// that is the picture the row keeps -- it is taken from the thumbnail track
/// and follows the cuts, which this cannot do.
pub fn glance(spec: &str, into: f64, width: u32) -> Result<Vec<u8>> {
    glance_now(spec, Landing::Fraction(into), width).map(|s| s.jpeg)
}

/// As [`glance`], asked for an instant rather than for a fraction of the way
/// in, and answering with the instant it actually landed on.
///
/// What the cut editor shows while the walk over the packets is still
/// running. A recording nothing has been read of has no access points to seek
/// by, so this asks libavformat for its own approximate seek and takes the
/// first picture that decodes after it -- which on a transport stream lands
/// near the instant asked for rather than on it, and can be a second or two
/// out. The time that comes back is the picture's own, so the frame counter
/// says where the picture really is rather than where it was asked for.
///
/// Costs one open, one seek and one GOP: tens of milliseconds, against the
/// second a gigabyte the walk takes to make an exact answer possible.
pub fn glance_at(spec: &str, at: f64, width: u32) -> Result<Shot> {
    glance_now(spec, Landing::At(at), width)
}

/// Where a glance is to land: a fraction of the way in, for a picture that
/// only has to stand for the recording, or a given instant in rebased
/// seconds, for one that has to stand for a moment in it.
enum Landing {
    Fraction(f64),
    At(f64),
}

/// Several glances out of one recording, at the times asked for and in that
/// order.
///
/// What the film strip draws with while the walk is still running. A glance
/// is an open, a seek and a GOP, and over a share the open is the dear part
/// of that -- a cell each would pay for it a dozen times over a single
/// refresh, against a recording the walk is already reading. So the open is
/// paid for once and the seeks happen inside it.
///
/// A time with no picture near it comes back empty rather than shifting the
/// ones after it along, exactly as in [`shots_at`]: the strip keeps the
/// playhead in its middle cell, and that only holds if the cells stay where
/// they were put.
pub fn glance_run(spec: &str, times: &[f64], width: u32) -> Result<Vec<Option<Shot>>> {
    if times.is_empty() {
        return Ok(Vec::new());
    }
    let mut g = Glancer::open(spec)?;
    Ok(times.iter().map(|&t| g.at(Landing::At(t), width).ok()).collect())
}

/// One picture out of each cell of a stretch, from a single seek and a read.
///
/// The other half of [`glance_run`], for a film strip drawn so close in that
/// the whole reel is a few seconds of the recording. A seek can only land on
/// an entry point, so a cell narrower than the recording's GOP cannot be
/// answered by seeking at all: broadcast material carries an entry point
/// every half second and some stations every whole second, against cells that
/// are a quarter of a second wide at the strip's closest setting. Reading the
/// stretch through decodes every picture in it, entry point or not, and **the
/// first picture in each cell is kept** -- so every cell of the reel is
/// answered, whatever the GOP length.
///
/// Taking any picture rather than only the entry ones is free: they are
/// decoded either way, on the path to the ones that are wanted. And it is no
/// less honest, because a strip drawn before the walk cannot say where a cut
/// is free in any case -- what a cell promises is a picture out of the stretch
/// it covers, and that is what this gives it. The picture's own kind comes
/// back alongside for a caller that cares.
///
/// Dear in proportion to the stretch rather than to the number of cells, so it
/// is only worth asking for while the stretch is short. The caller picks; see
/// `fillByGlance`.
///
/// `cell` is how the caller has divided the stretch up, and a picture landing
/// in a cell already answered is skipped: the caller has nowhere to put a
/// second one, and a disc puts an entry point at every scene change -- on one
/// of them they come 0.067s apart, a hundred and twenty pictures across a reel
/// with room for eleven. Zero divides the stretch into `most` of them instead.
///
/// **Cells, not a minimum spacing.** Skipping a picture that came within some
/// distance of the last one looks like the same thing and is not: pictures
/// half a second apart, thinned to "no closer than 0.54s", come back one
/// second apart -- and a strip whose cells are 0.6s wide is then empty every
/// other cell, which is the very thing this is here to fix.
pub fn glance_sweep(
    spec: &str,
    from: f64,
    to: f64,
    width: u32,
    cell: f64,
    most: usize,
) -> Result<Vec<Shot>> {
    if to <= from || most == 0 {
        return Ok(Vec::new());
    }
    Glancer::open(spec)?.sweep(from, to, width, cell, most)
}

fn glance_now(spec: &str, landing: Landing, width: u32) -> Result<Shot> {
    Glancer::open(spec)?.at(landing, width)
}

/// A recording nothing has been read of, open and ready to be asked for a
/// picture from around an instant.
struct Glancer {
    /// The name it was asked for by, for what the failures say.
    spec: String,
    ictx: ff::format::context::Input,
    decoder: ff::decoder::Video,
    idx: usize,
    time_base: f64,
    sar: f64,
    /// The container's own clock, which is what a landing is measured
    /// against: where the recording says it begins, and how long it says it
    /// runs.
    start: f64,
    duration: f64,
    /// Whether a glance has been taken out of this one already, which is what
    /// says the file has to be wound back before one is taken from the front.
    used: bool,
}

impl Glancer {
    fn open(spec: &str) -> Result<Self> {
        crate::init()?;
        let input = crate::input::Input::parse(spec)?;
        let ictx =
            crate::input::demux(&input.url).map_err(|e| anyhow!("cannot open {spec}: {e}"))?;
        let stream = ictx
            .streams()
            .best(ff::media::Type::Video)
            .ok_or_else(|| anyhow!("no video stream in {spec}"))?;
        let idx = stream.index();
        let time_base = f64::from(stream.time_base());
        let params = stream.parameters();
        let sar = unsafe {
            let s = (*params.as_ptr()).sample_aspect_ratio;
            if s.num > 0 && s.den > 0 { s.num as f64 / s.den as f64 } else { 1.0 }
        };
        let (duration, start) = unsafe {
            let p = ictx.as_ptr();
            let tb = ff::ffi::AV_TIME_BASE as f64;
            let d = (*p).duration;
            let s = (*p).start_time;
            (
                if d == ff::ffi::AV_NOPTS_VALUE { 0.0 } else { d as f64 / tb },
                if s == ff::ffi::AV_NOPTS_VALUE { 0.0 } else { s as f64 / tb },
            )
        };
        // One core, because this runs beside the passes that want the rest of
        // them, and because frame threading holds the first pictures back
        // until its pipeline fills -- and the first picture after a seek is
        // the whole of what this wants. See [`crate::video_decoder_with`].
        let decoder = crate::video_decoder_with(params, 1)?;
        Ok(Glancer {
            spec: spec.to_string(),
            ictx,
            decoder,
            idx,
            time_base,
            sar,
            start,
            duration,
            used: false,
        })
    }

    /// Seek, decode the first picture that stands on its own, and answer with
    /// it and the instant it turned out to be at.
    fn at(&mut self, landing: Landing, width: u32) -> Result<Shot> {
        // Field by field, so that the packet loop can hold the demuxer while
        // the decoder is fed: they are separate places and the borrow checker
        // will only see that if it is told them separately.
        let Glancer { spec, ictx, decoder, idx, time_base, sar, start, duration, used } = self;
        let (idx, time_base, sar, start) = (*idx, *time_base, *sar, *start);
        let again = std::mem::replace(used, true);

        // A recording whose length the container will not say is read from the
        // beginning: there is no fraction to take of nothing.
        let want = match landing {
            Landing::Fraction(into) => start + duration.max(0.0) * into.clamp(0.0, 1.0),
            // Rebased seconds in, container time out, as everywhere else.
            Landing::At(at) => start + at.max(0.0),
        };
        // Whatever the last glance left in the pipeline belongs to wherever it
        // landed, and is a picture from the wrong place if it comes out here.
        decoder.flush();
        if want > start + 1e-6 {
            let ts = (want * ff::ffi::AV_TIME_BASE as f64) as i64;
            // A seek that fails leaves the file where it was, which is the
            // start -- a picture from the wrong place, and not a reason to
            // have none.
            let _ = ictx.seek(ts, ..ts);
        } else if again {
            // A landing at the very front needs no seek out of a file that has
            // only just been opened -- but on a second glance the file is
            // wherever the first one left it, and has to be wound back.
            let _ = ictx.seek(0, ..0);
        }

        let mut frame = ff::frame::Video::empty();
        // The picture to answer with, which is the first I picture: what comes
        // out before it references pictures the seek skipped past, and decodes
        // to a grey field with the corner of a logo in it. Nothing is fed to
        // the decoder until a key packet has arrived for the same reason --
        // the seek lands near the entry point on a transport stream rather
        // than on it.
        //
        // What was decoded anyway is kept as the answer of last resort, for a
        // recording whose pictures are not typed at all: a poster that is a
        // little wrong beats a row that stays blank.
        let mut spare: Option<Shot> = None;
        let mut started = false;
        let mut waiting = 0;
        // Enough to carry a landing that fell inside an open GOP, and to give
        // up on a stretch of the file that decodes to nothing rather than
        // reading to the end of it.
        let mut left = 900;
        // The picture's own instant, rebased, so the caller can say where what
        // it is looking at actually is.
        let when = |frame: &ff::frame::Video| {
            frame.pts().map_or(0.0, |pts| pts as f64 * time_base - start)
        };
        for (s, packet) in ictx.packets() {
            if s.index() != idx {
                continue;
            }
            started = started || packet.is_key();
            if !started {
                // A recording that flags no key packet at all -- and there are
                // containers that flag none -- is read from wherever the seek
                // put it rather than to the end and answered with nothing.
                waiting += 1;
                started = waiting > 300;
                if !started {
                    continue;
                }
            }
            left -= 1;
            if left <= 0 {
                break;
            }
            if decoder.send_packet(&packet).is_err() {
                continue;
            }
            while decoder.receive_frame(&mut frame).is_ok() {
                let kind = kind_of(&frame);
                if kind == "I" {
                    return Ok(Shot {
                        jpeg: encode_jpeg(&frame, sar, width)?,
                        time: when(&frame),
                        kind,
                    });
                }
                if spare.is_none() {
                    spare = Some(Shot {
                        jpeg: encode_jpeg(&frame, sar, width)?,
                        time: when(&frame),
                        kind,
                    });
                }
            }
        }
        // Whatever the decoder was still holding: a recording shorter than the
        // reorder depth has all of its pictures in there.
        let _ = decoder.send_eof();
        while decoder.receive_frame(&mut frame).is_ok() {
            let kind = kind_of(&frame);
            if kind == "I" {
                return Ok(Shot {
                    jpeg: encode_jpeg(&frame, sar, width)?,
                    time: when(&frame),
                    kind,
                });
            }
            if spare.is_none() {
                spare = Some(Shot {
                    jpeg: encode_jpeg(&frame, sar, width)?,
                    time: when(&frame),
                    kind,
                });
            }
        }
        spare.ok_or_else(|| match landing {
            Landing::Fraction(into) => anyhow!("no picture {:.0}% into {spec}", into * 100.0),
            Landing::At(at) => anyhow!("no picture at {at:.3}s in {spec}"),
        })
    }

    /// Seek to `from` and read through to `to`, keeping every entry picture on
    /// the way. See [`glance_sweep`].
    fn sweep(
        &mut self,
        from: f64,
        to: f64,
        width: u32,
        cell: f64,
        most: usize,
    ) -> Result<Vec<Shot>> {
        let Glancer { ictx, decoder, idx, time_base, sar, start, used, .. } = self;
        let (idx, time_base, sar, start) = (*idx, *time_base, *sar, *start);
        *used = true;

        decoder.flush();
        let want = start + from.max(0.0);
        let ts = (want * ff::ffi::AV_TIME_BASE as f64) as i64;
        let _ = ictx.seek(ts, ..ts);

        let mut out: Vec<Shot> = Vec::new();
        // Which of the caller's cells the last answer went into.
        let mut held = i64::MIN;
        let wide = if cell > 1e-9 { cell } else { (to - from) / most as f64 };
        let cell_of = move |t: f64| ((t - from) / wide).floor() as i64;
        let mut frame = ff::frame::Video::empty();
        // Nothing is fed to the decoder until a key packet has arrived: what
        // comes out before one references pictures the seek skipped past.
        let mut started = false;
        // A ceiling on the read, for a recording whose pictures carry no time
        // to stop at. The stretch this is asked for is seconds long, and a
        // second is tens of packets.
        let mut left = 4000;
        let when = |frame: &ff::frame::Video| {
            frame.pts().map_or(0.0, |pts| pts as f64 * time_base - start)
        };
        'read: for (s, packet) in ictx.packets() {
            if s.index() != idx {
                continue;
            }
            started = started || packet.is_key();
            if !started {
                continue;
            }
            left -= 1;
            if left <= 0 {
                break;
            }
            if decoder.send_packet(&packet).is_err() {
                continue;
            }
            while decoder.receive_frame(&mut frame).is_ok() {
                let at = when(&frame);
                // Pictures come out of the decoder in presentation order, so
                // the first one past the far end says the stretch is done.
                if at > to + 1e-6 {
                    break 'read;
                }
                if at < from - 1e-6 {
                    continue;
                }
                // One picture per cell is all the caller can show, and the
                // decoding is done either way -- what this saves is the
                // encoding of the ones that would land on top of each other.
                let k = cell_of(at);
                if k == held {
                    continue;
                }
                held = k;
                out.push(Shot {
                    jpeg: encode_jpeg(&frame, sar, width)?,
                    time: at,
                    kind: kind_of(&frame),
                });
                if out.len() >= most {
                    break 'read;
                }
            }
        }
        Ok(out)
    }
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

/// `sar` is the recording's pixel aspect ratio -- see [`crate::VideoInfo`].
/// Taken as a number rather than off a [`Source`], because a picture can be
/// wanted before there is one; see [`glance`].
pub(crate) fn encode_jpeg(
    picture: &ff::frame::Video,
    sar: f64,
    width: u32,
) -> Result<Vec<u8>> {
    let sar = sar.max(0.01);
    // What the picture is worth, in square pixels. Not its coded width: 1440
    // samples across shown at 16:9 needs 1920 to keep all 1080 of its lines,
    // and stopping at 1440 would throw a quarter of them away. Past that
    // there is nothing more to ask for -- the stage asks for its own pixels
    // and can be wider than the source, and a bigger number there only makes
    // a bigger JPEG out of the same samples. It is the proxy that this
    // usually measures, and a proxy is square-pixel: its width *is* the
    // ceiling on everything the timeline shows.
    let native = (picture.width() as f64 * sar).round() as u32;
    let out_w = width.min(native).max(16) & !1;
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
/// `keys` hands over only the pictures that open a GOP, skipping everything
/// between them unparsed -- for callers that are asking about entry points
/// and nothing else. `visit` returns false to stop.
fn walk(
    src: &Source,
    from: f64,
    margin: f64,
    keys: bool,
    mut visit: impl FnMut(f64, &ff::frame::Video) -> bool,
) -> Result<Option<f64>> {
    let mut ictx = crate::input::demux(&src.input.url)?;
    let idx = src.video.stream_index;
    let in_tb = src.video.time_base;
    // The index knows the byte `from` begins at, so on the first attempt
    // there is nothing to approximate and no margin to spend. The timestamp
    // seek below is what is left for the containers and indexes that cannot
    // say -- and for the second attempt, which only happens now when the
    // first somehow still landed late.
    let placed = margin == 0.0 && crate::index::seek_to_entry(&mut ictx, src, from).is_some();
    if !placed {
        let landing = (from - margin).max(0.0);
        // Asking for the beginning has to mean the beginning, exactly as in
        // `cut::seek_to`: aiming at the container's own start time lands
        // *past* the file's first entry point, and that is the one place the
        // back-off below has nothing earlier to fall back to. A recording
        // whose first entry point sits at time zero -- which is most of them
        // once the times are rebased -- could not have its opening pictures
        // read at all, and the film strip drew its first cell blank while a
        // proxy was being built.
        //
        // The threshold is that first entry point rather than zero, because
        // nothing before it can be decoded anyway: aiming at it would land
        // past it and cost a whole second pass to find that out.
        let target = if src.points.first().is_none_or(|p| landing <= p.time) {
            i64::MIN / 2
        } else {
            ((landing + src.start_time) * ff::ffi::AV_TIME_BASE as f64) as i64
        };
        let _ = ictx.seek(target, ..target);
    }

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
        // Never handed over rather than decoded and dropped -- and filtered
        // here rather than with `skip_frame`, which is per *picture* and so
        // takes the P bottom field off a field-coded entry point; see the
        // note in `thumbs::build`.
        if keys && !packet.is_key() {
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
