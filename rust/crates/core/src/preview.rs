//! Single-frame extraction, for scrubbing a timeline.
//!
//! A webview cannot play MPEG-2 in a transport stream, so the picture under
//! the playhead has to be decoded here and handed over as an image.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;
use crate::input::ReadPackets;

use crate::{AccessPoint, Source};
use std::rc::Rc;

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
    let jump = if base.is_finite() {
        (base * 2.5 + 0.5).min(WALK)
    } else {
        f64::INFINITY
    };

    // Every cell of a GOP-divided film strip stands on an entry point, and
    // when they all do, everything between them can go by unparsed: an entry
    // picture decodes on its own. That is the difference between decoding the
    // six seconds a run covers and decoding the thirteen pictures wanted out
    // of it, which is what the strip costs while a proxy is being built and
    // there is nothing faster to read from.
    let keys = times.iter().all(|&t| on_point(&src.points, t, fd / 2.0));

    // One pool for the whole request, not one per run. The strip's wider
    // settings ask for a run per cell -- the cells are further apart than a
    // run is walked through -- and a pool costs a decoder per worker to open,
    // which on 4K is not something to do a dozen times over. Opened on the
    // first run rather than here, out of the demuxer that run had to open
    // anyway: opening one only to read the stream's parameters off it is a
    // quarter of a second on a transport stream, which is most of what a
    // refresh is allowed.
    let mut pool: Option<StripPool> = None;

    let mut out: Vec<Option<Shot>> = (0..times.len()).map(|_| None).collect();
    let mut start = 0usize;
    for i in 1..=times.len() {
        let split = i == times.len() || times[i] - times[i - 1] > jump || times[i] < times[i - 1];
        if !split {
            continue;
        }
        let decoding = keys.then_some(&mut pool);
        for (k, shot) in collect_run(src, &times[start..i], width, fd, decoding)?
            .into_iter()
            .enumerate()
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

/// What a stretch of a recording's entry pictures amounts to before any of
/// them is decoded: what to build a decoder from, where the first of them
/// presents, and the packets of each picture that was asked to be kept,
/// against its own instant.
type EntryRun = (ff::codec::Parameters, Option<f64>, Vec<(f64, Vec<ff::Packet>)>);

/// What the film strip's own decoder pool hands back: a picture's instant,
/// what kind it is, and the image itself, made on the worker that decoded it.
type StripPool = crate::entrypool::Pool<(f64, &'static str, Vec<u8>)>;

/// The slots one run of the film strip is being filled into.
///
/// Every slot wants the picture nearest its own instant and will not take one
/// further off than `window`, so a picture is offered to all of them and the
/// ones it improves keep a copy. A slot with no picture near it stays empty
/// rather than borrowing its neighbour's: the strip keeps the playhead in its
/// middle cell, and that only holds if the cells stay where they were put.
/// `P` is whatever the caller is filling them with: a decoded frame where the
/// run is walked through, an encoded image where it came off the pool, which
/// made it on the worker. Encoding is what [`Self::finish`] is for, and it
/// happens once per slot: a slot in a walked run is replaced by every nearer
/// picture that goes past, and encoding on the way in cost one for each of
/// them.
struct Slots<'a, P> {
    wanted: &'a [f64],
    window: f64,
    got: Vec<Option<(f64, &'static str, P)>>,
}

impl<'a, P: Clone> Slots<'a, P> {
    fn new(wanted: &'a [f64], window: f64) -> Self {
        Self {
            wanted,
            window,
            got: (0..wanted.len()).map(|_| None).collect(),
        }
    }

    /// Whether any slot would take a picture at `t`.
    ///
    /// Exact rather than a guess: a slot is only ever filled by a picture
    /// inside its window, and only ever replaced by a nearer one, so a
    /// picture outside every window cannot be taken by anything. Asked before
    /// a picture is decoded at all, where the caller is in a position to
    /// choose.
    fn open_to(&self, t: f64) -> bool {
        self.wanted.iter().any(|w| (t - w).abs() <= self.window)
    }

    /// Offer a picture. `hold` is asked for only if a slot takes it.
    fn offer(
        &mut self,
        t: f64,
        kind: &'static str,
        mut hold: impl FnMut() -> Result<P>,
    ) -> Result<()> {
        let mut made: Option<P> = None;
        for (i, &w) in self.wanted.iter().enumerate() {
            let better = match &self.got[i] {
                Some((have, _, _)) => (t - w).abs() < (have - w).abs(),
                None => (t - w).abs() <= self.window,
            };
            if !better {
                continue;
            }
            let held = match &made {
                Some(p) => p.clone(),
                None => made.insert(hold()?).clone(),
            };
            self.got[i] = Some((t, kind, held));
        }
        Ok(())
    }

    /// The run's answer, once nothing more is coming.
    fn finish(
        mut self,
        fd: f64,
        image: impl Fn(P) -> Result<Vec<u8>>,
    ) -> Result<Vec<Option<Shot>>> {
        // Two slots can round to the same picture -- the strip's spacing need
        // not be a whole number of picture intervals, and under pulldown it
        // never is. The later slot gives it up rather than repeating it.
        for i in (1..self.got.len()).rev() {
            let dup = match (&self.got[i], &self.got[i - 1]) {
                (Some(a), Some(b)) => (a.0 - b.0).abs() < fd / 2.0,
                _ => false,
            };
            if dup {
                self.got[i] = None;
            }
        }
        self.got
            .into_iter()
            .map(|slot| match slot {
                Some((time, kind, held)) => Ok(Some(Shot {
                    jpeg: image(held)?,
                    time,
                    kind,
                })),
                None => Ok(None),
            })
            .collect()
    }
}

/// One seek, then a straight decode filling every slot with the nearest
/// picture to it.
fn collect_run(
    src: &Source,
    wanted: &[f64],
    width: u32,
    fd: f64,
    pool: Option<&mut Option<StripPool>>,
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
    if let Some(pool) = pool {
        return entry_run(src, wanted, width, fd, from, window, pool);
    }

    let sar = src.video.sample_aspect_ratio;
    let mut got = Slots::new(wanted, window);
    for (attempt, margin) in [0.0, src.seek_margin].into_iter().enumerate() {
        let mut slots = Slots::new(wanted, window);
        let mut failed = None;
        let began = walk(src, from, margin, false, true, Cores::One, false, None, |t, frame| {
            if t > last + fd {
                return false;
            }
            if let Err(e) = slots.offer(t, kind_of(frame), || Ok(frame.clone())) {
                failed = Some(e);
                return false;
            }
            true
        })?;
        if let Some(e) = failed {
            return Err(e);
        }
        got = slots;
        if !landed_late(began, first, window / 2.0) || attempt == 1 {
            break;
        }
    }
    got.finish(fd, |frame| encode_jpeg(&frame, sar, width))
}

/// As [`collect_run`], for a run whose every slot stands on an entry point.
///
/// **The pictures are decoded on every core**, which is the whole reason this
/// is a path of its own: an entry picture decodes on its own, and the strip
/// asks for a dozen or more of them at once. Decoded one at a time a refresh
/// costs a picture apiece -- 28 ms each on the recorder's 4K material, so
/// four hundred milliseconds of strip standing still behind a playhead that
/// has moved on, which is exactly how it looked. See [`crate::entrypool`].
///
/// Nothing else about the run changes: the packets are gathered in the order
/// the file holds them, the answers are put back in that order, and the slots
/// are filled from them exactly as [`collect_run`] fills them.
fn entry_run(
    src: &Source,
    wanted: &[f64],
    width: u32,
    fd: f64,
    from: f64,
    window: f64,
    pool: &mut Option<StripPool>,
) -> Result<Vec<Option<Shot>>> {
    let first = wanted[0];
    let last = *wanted.last().unwrap();
    let mut got = Slots::new(wanted, window);
    for (attempt, margin) in [0.0, src.seek_margin].into_iter().enumerate() {
        let mut slots = Slots::new(wanted, window);
        // Only the pictures a slot could take are even held on to. At the
        // strip's wider settings a run is a single cell and the seek lands a
        // GOP or more before it, so this is most of what goes past -- and at
        // 4K a packet nobody wants is a megabyte nobody wants.
        let (params, began, run) =
            entry_packets(src, from, margin, last + fd, |t| slots.open_to(t))?;
        // Nothing found. A seek that landed past the whole run is what the
        // second attempt is for, and opening a pool for it would fix its
        // width at one worker for the attempt that does find something.
        if run.is_empty() {
            got = slots;
            if !landed_late(began, first, window / 2.0) || attempt == 1 {
                break;
            }
            continue;
        }
        if pool.is_none() {
            let sar = src.video.sample_aspect_ratio;
            // No wider than this run has pictures to decode. A pool costs a
            // decoder and a thread per worker to open, and at the strip's
            // wider settings a run is a single cell: sixteen decoders opened
            // to decode one picture cost more than the picture does, and on
            // MPEG-2 that showed up as a refresh a third slower than before
            // there was a pool at all. Later runs of the same request are
            // about the same size, so the first one's width fits them.
            let workers = crate::entrypool::width(0, &src.video).min(run.len());
            *pool = Some(crate::entrypool::Pool::new(
                params,
                &src.video,
                src.start_time,
                workers,
                // In front, unlike the pass that builds the track. This pool
                // is the strip under the pointer: it exists because a refresh
                // that takes 400 ms is a strip standing still behind a
                // playhead that has moved on, and standing aside for the rest
                // of the machine is exactly how it would take 400 ms again.
                crate::entrypool::Standing::Front,
                move |t, frame| Ok((t, kind_of(frame), encode_jpeg(frame, sar, width)?)),
            )?);
        }
        let pool = pool.as_mut().expect("just made");
        for (_, packets) in run {
            pool.push(packets);
            for made in std::iter::from_fn(|| pool.ready()) {
                for (t, kind, jpeg) in made? {
                    slots.offer(t, kind, || Ok(jpeg.clone()))?;
                }
            }
        }
        for made in pool.drain() {
            for (t, kind, jpeg) in made? {
                slots.offer(t, kind, || Ok(jpeg.clone()))?;
            }
        }
        got = slots;
        if !landed_late(began, first, window / 2.0) || attempt == 1 {
            break;
        }
    }
    // Already encoded, on the worker that decoded it.
    got.finish(fd, Ok)
}

/// The entry pictures of a stretch of the recording, as packets, decoding
/// none of them.
///
/// Answers with the instant the first of them presents at as well, which is
/// what says whether the seek landed past what was asked for -- an entry
/// picture presents at its own packet's timestamp, so this is the same answer
/// [`walk`] gives by decoding one -- and with what a decoder for them has to
/// be built from, so that nothing else has to open the recording to find out.
fn entry_packets(
    src: &Source,
    from: f64,
    margin: f64,
    until: f64,
    keep: impl Fn(f64) -> bool,
) -> Result<EntryRun> {
    let mut ictx = crate::input::demux(&src.input.url)?;
    place(&mut ictx, src, from, margin)?;
    let idx = src.video.stream_index;
    // After the seek, for the reason [`crate::input::keep_only`] gives.
    crate::input::keep_only(&mut ictx, &[idx]);
    let params = ictx
        .stream(idx)
        .ok_or_else(|| anyhow!("stream {idx} vanished"))?
        .parameters();
    let mut entries = crate::EntryPictures::new(&src.video);
    let mut out: Vec<(f64, Vec<ff::Packet>)> = Vec::new();
    let mut first = None;
    let mut pending: Vec<ff::Packet> = Vec::new();
    let mut at: Option<f64> = None;
    let mut stretches = Stretches::new(src);
    for (stream, packet) in ictx.read_packets() {
        if stream.index() != idx {
            continue;
        }
        stretches.reach(packet.position());
        let step = entries.step(&packet);
        if step == crate::Step::Skip {
            continue;
        }
        // The second field of a pair often carries no timestamp of its own,
        // so the picture is timed by the first packet of it that has one --
        // and a packet without one is still half a picture and stays with its
        // partner. Dropping it on the spot would leave the partner to be read
        // as a picture of its own, which decodes to nothing.
        if at.is_none() {
            at = packet
                .pts()
                .map(|pts| pts as f64 * src.video.time_base - src.start_time);
        }
        pending.push(packet);
        if step == crate::Step::Half {
            continue;
        }
        let Some(when) = at.take() else {
            pending.clear();
            continue;
        };
        // Not a picture of the stretch it was read in, as [`walk`] passes
        // it over: neither shown nor counted as where the read landed.
        if stretches.outside(when) {
            pending.clear();
            continue;
        }
        // Set before the run is cut short, exactly as `walk` sets it: a
        // landing past everything asked for is what says the seek has to be
        // tried again, and that has to be reported rather than dropped.
        if first.is_none() {
            first = Some(when);
        }
        if when > until {
            break;
        }
        let packets = std::mem::take(&mut pending);
        if keep(when) {
            out.push((when, packets));
        }
    }
    Ok((params, first, out))
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
///
/// **The stretch is `[from, until)`, which is what a cut of it covers.** The
/// picture at `until` is the first one the cut took away, and the editor
/// counts it that way too -- see `srcToOut` in the window. Played, it was a
/// picture of the material that had just gone, standing at a join; and
/// because it fell due at exactly the moment the next range's first picture
/// did, the window's pacing kept it and dropped that one. So a join played
/// back showed a frame of what was cut and never showed the frame that was
/// actually joined to.
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
        let first = walk(src, entry, margin, false, true, Cores::All, false, None, |t, frame| {
            if t >= until - 1e-6 {
                // The end of the stretch, which stops the playing -- unless
                // nothing of it has been played: a seek that landed past the
                // whole of a short range is the late landing below, and is
                // tried again from further back rather than played as nothing.
                stopped = !began_late;
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
    let (at, picture) = picture_at(src, time)?;
    let kind = kind_of(&picture);
    Ok(Shot {
        jpeg: encode_jpeg(&picture, src.video.sample_aspect_ratio, width)?,
        time: at,
        kind,
    })
}

/// The decoded picture shown at `time`, and the instant it really stands at.
///
/// What [`shot_at`] is before it encodes. Handed over undecided, because a
/// caller that is going to composite it with another picture -- see
/// [`crate::crossview`] -- has to do that while it is still samples: a JPEG
/// of one side and a JPEG of the other cannot be blended into a picture
/// neither of them is.
pub fn picture_at(src: &Source, time: f64) -> Result<(f64, ff::frame::Video)> {
    picture_in(src, time, crate::weave::woven(src))
}

/// As [`picture_at`], saying whether a recording that repeats fields is to be
/// woven into the frames a screen shows (see [`crate::weave`]) or answered
/// with the coded picture nearest `time`.
fn picture_in(src: &Source, time: f64, woven: bool) -> Result<(f64, ff::frame::Video)> {
    crate::init()?;
    let fd = src.video.frame_duration();
    let from = entry_before(&src.points, time);
    // Woven frames sit exactly a frame apart, so the frame wanted is the one
    // the instant falls *in*, not the nearer of two: an instant half way
    // between two of them is where an access point stands when its picture
    // begins on the second field of a frame, and the nearer-of-two rule sent
    // it to the frame after -- the one past the point. See [`crate::weave`].
    let mut picture: Option<(f64, ff::frame::Video)> = None;
    for (attempt, margin) in [0.0, src.seek_margin].into_iter().enumerate() {
        let mut hit: Option<(f64, ff::frame::Video)> = None;
        let mut tail: Option<(f64, ff::frame::Video)> = None;
        let began = walk(src, from, margin, false, woven, Cores::One, false, None, |t, frame| {
            // The wanted picture is whichever of the two straddling `time` is
            // nearer -- not "the first one at or after it". Under 2:3
            // pulldown the pictures are 41.7ms apart inside a 29.97 fps
            // stream, so assuming a picture every frame duration picks the
            // wrong side of the gap.
            if woven {
                if t > time + fd / 4.0 {
                    // Nothing before it: the walk began past the instant, and
                    // the first frame is the answer, as it is below.
                    hit = tail.take().or_else(|| Some((t, frame.clone())));
                    return false;
                }
                tail = Some((t, frame.clone()));
                return true;
            }
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
        // `began` is the first picture's own instant, and woven, the first
        // frame is the one that picture's first field is in -- half a frame
        // before it where the picture begins on a second field. That frame is
        // the answer for an instant up to a quarter of a frame past its start,
        // as above. Asked of the picture's instant with half a frame of slack,
        // an access point named by its frame was a tick over it on every other
        // one (a field is 1501 or 1502 ticks), and each was decoded twice.
        let late = if woven {
            landed_late(began.map(|b| crate::weave::on_frame(src, b)), time, fd / 4.0)
        } else {
            landed_late(began, time, fd / 2.0)
        };
        if !late || attempt == 1 {
            picture = hit.or(tail);
            break;
        }
    }

    picture.ok_or_else(|| anyhow!("no picture at {time:.3}s"))
}

/// One decoded picture as a JPEG, at most `width` across.
///
/// [`encode_jpeg`] under a name the rest of the program may say. `sar` is
/// the recording's pixel aspect ratio; a picture already brought to square
/// pixels passes 1.0.
pub fn jpeg_of(picture: &ff::frame::Video, sar: f64, width: u32) -> Result<Vec<u8>> {
    encode_jpeg(picture, sar, width)
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
    Ok(times
        .iter()
        .map(|&t| g.at(Landing::At(t), width).ok())
        .collect())
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
    ictx: crate::input::Demux,
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
        let mut ictx =
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
            if s.num > 0 && s.den > 0 {
                s.num as f64 / s.den as f64
            } else {
                1.0
            }
        };
        // Read before `keep_only` switches the other streams off, as the
        // walk's own are: the start is the earliest stream's. See
        // [`crate::container_start`].
        let (duration, start) = (crate::container_duration(&ictx), crate::container_start(&ictx));
        // One core, because this runs beside the passes that want the rest of
        // them, and because frame threading holds the first pictures back
        // until its pipeline fills -- and the first picture after a seek is
        // the whole of what this wants. See [`crate::video_decoder_with`].
        let decoder = crate::video_decoder_with(params, 1)?;
        // Only the pictures, which is all this reads -- and this one seeks
        // again and again, so the stream the seek is aimed by has to be one
        // that stays. That is the pictures. See [`crate::input::keep_only`].
        crate::input::keep_only(&mut ictx, &[idx]);
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
        let Glancer {
            spec,
            ictx,
            decoder,
            idx,
            time_base,
            sar,
            start,
            duration,
            used,
        } = self;
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
            frame
                .pts()
                .map_or(0.0, |pts| pts as f64 * time_base - start)
        };
        for (s, packet) in ictx.read_packets() {
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
        let Glancer {
            ictx,
            decoder,
            idx,
            time_base,
            sar,
            start,
            used,
            ..
        } = self;
        let (idx, time_base, sar, start) = (*idx, *time_base, *sar, *start);
        *used = true;

        decoder.flush();
        let want = start + from.max(0.0);
        // A stretch from the very front is read from the front, as `place`
        // does it: a seek aimed at the container's own start lands past the
        // first entry picture on a transport stream, and the reel's first
        // cell came back holding the picture after it.
        let ts = if want > start + 1e-6 {
            (want * ff::ffi::AV_TIME_BASE as f64) as i64
        } else {
            i64::MIN / 2
        };
        let _ = ictx.seek(ts, ..ts);

        let mut out: Vec<Shot> = Vec::new();
        // Which of the caller's cells the last answer went into.
        let mut held = i64::MIN;
        let wide = if cell > 1e-9 {
            cell
        } else {
            (to - from) / most as f64
        };
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
            frame
                .pts()
                .map_or(0.0, |pts| pts as f64 * time_base - start)
        };
        'read: for (s, packet) in ictx.read_packets() {
            if s.index() != idx {
                continue;
            }
            // Counted before the wait for a key packet as well as after it:
            // a container that flags none never ends that wait, and the
            // strip read to the end of the file on every update.
            left -= 1;
            if left <= 0 {
                break;
            }
            started = started || packet.is_key();
            if !started {
                continue;
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
pub(crate) fn encode_jpeg(picture: &ff::frame::Video, sar: f64, width: u32) -> Result<Vec<u8>> {
    encode_jpeg_sized(picture, sar, width).map(|(jpeg, _, _)| jpeg)
}

/// A pixel aspect ratio a picture can be drawn at. The container's is taken
/// as it says, and a crafted one of 1:1000 made every still a thousand times
/// its height -- hundreds of megabytes a picture, a strip's worth at once. No
/// recording has a pixel more than a few times wider than it is tall, nor a
/// field of one.
fn credible_sar(sar: f64) -> f64 {
    if sar.is_finite() {
        sar.clamp(0.125, 8.0)
    } else {
        1.0
    }
}

/// As [`encode_jpeg`], with the size the JPEG came out at.
fn encode_jpeg_sized(picture: &ff::frame::Video, sar: f64, width: u32) -> Result<(Vec<u8>, u32, u32)> {
    let native = (picture.width() as f64 * credible_sar(sar)).round() as u32;
    encode_jpeg_within(picture, sar, width.min(native))
}

/// `encode_jpeg_sized` with the width already settled by the caller: a field
/// stands for a frame twice its height, and is worth the frame's width.
fn encode_jpeg_within(picture: &ff::frame::Video, sar: f64, width: u32) -> Result<(Vec<u8>, u32, u32)> {
    let sar = credible_sar(sar);
    // What the picture is worth, in square pixels. Not its coded width: 1440
    // samples across shown at 16:9 needs 1920 to keep all 1080 of its lines,
    // and stopping at 1440 would throw a quarter of them away. Past that
    // there is nothing more to ask for -- the stage asks for its own pixels
    // and can be wider than the source, and a bigger number there only makes
    // a bigger JPEG out of the same samples. It is the proxy that this
    // usually measures, and a proxy is square-pixel: its width *is* the
    // ceiling on everything the timeline shows.
    let out_w = width.max(16) & !1;
    // Downscaling far enough also takes the comb out of interlaced material,
    // so a preview needs no deinterlacer of its own.
    let out_h = (((out_w as f64 * picture.height() as f64) / (picture.width() as f64 * sar)).round()
        as u32)
        .max(16) & !1;

    // Told the picture's range rather than left to read it off the format:
    // a full-range picture in a plain one -- HEVC or 10-bit tagged pc -- was
    // taken for studio and stretched a second time, its blacks crushed and
    // its whites blown in every still the editor and the strip showed.
    let mut scaler = crate::blend::Scaler::with_flags(
        (picture.width(), picture.height(), picture.format(), crate::blend::full_range_frame(picture)),
        (out_w, out_h, ff::format::Pixel::YUVJ420P, true),
        ff::software::scaling::Flags::AREA,
    )?;
    let mut scaled = ff::frame::Video::new(ff::format::Pixel::YUVJ420P, out_w, out_h);
    scaler.run(picture, &mut scaled)?;

    let codec =
        ff::encoder::find(ff::codec::Id::MJPEG).ok_or_else(|| anyhow!("no MJPEG encoder"))?;
    let mut enc = ff::codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()?;
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
    Ok((out, out_w, out_h))
}

/// The widest a cover is made. A file manager shows it a few hundred pixels
/// across; this is room for a player that shows it whole.
const POSTER_WIDTH: u32 = 1920;

/// The picture shown at `time`, as a cover for the file a cut writes. See
/// [`crate::cut::Poster`].
///
/// Full size rather than a preview's, and from one field where the picture
/// is interlaced: a still is looked at, and two fields a sixtieth of a
/// second apart woven into one are a comb along everything that moved.
///
/// Not woven where the recording repeats fields: two of every five frames
/// woven out of 2:3 pulldown are fields of two different pictures -- a comb
/// just the same, on a frame that does not say it is interlaced. The coded
/// picture nearest `time` is a whole one.
pub fn poster_at(src: &Source, time: f64) -> Result<crate::cut::Poster> {
    let (_, picture) = picture_in(src, time, false)?;
    // Held to one a picture can have before it is halved for a field, as a
    // still's is: clamped only after halving, a crafted 1:1000 gave a cover
    // 19 pixels wide, and a crafted 1000:1 one half as tall as the frame's.
    let sar = credible_sar(src.video.sample_aspect_ratio);
    let (jpeg, width, height) = if picture.is_interlaced() && picture.height() >= 32 {
        // Half the lines, each standing for two: twice as tall a pixel. Sized
        // as the frame it stands for (1080i gives 1920x1080, not 960x540):
        // the field has every sample across, only the lines are halved.
        let native = (picture.width() as f64 * sar).round() as u32;
        encode_jpeg_within(&top_field(&picture), sar / 2.0, POSTER_WIDTH.min(native))?
    } else {
        encode_jpeg_sized(&picture, sar, POSTER_WIDTH)?
    };
    Ok(crate::cut::Poster { jpeg, width, height })
}

/// The picture's top field: its even lines, in every plane.
fn top_field(picture: &ff::frame::Video) -> ff::frame::Video {
    let mut field = ff::frame::Video::new(picture.format(), picture.width(), picture.height() / 2);
    field.set_color_range(picture.color_range());
    field.set_color_space(picture.color_space());
    field.set_color_primaries(picture.color_primaries());
    field.set_color_transfer_characteristic(picture.color_transfer_characteristic());
    for plane in 0..picture.planes().min(field.planes()) {
        let (from, to) = (picture.stride(plane), field.stride(plane));
        let (rows, have) = (field.plane_height(plane) as usize, picture.plane_height(plane) as usize);
        let n = from.min(to);
        let src = picture.data(plane);
        let dst = field.data_mut(plane);
        for y in 0..rows {
            let line = 2 * y;
            if line >= have || (line + 1) * from > src.len() || (y + 1) * to > dst.len() {
                break;
            }
            dst[y * to..y * to + n].copy_from_slice(&src[line * from..line * from + n]);
        }
    }
    field
}

/// The image formats a frame can be saved in, by the extension of the name
/// it is saved under.
pub const STILL_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "bmp"];

/// Save the picture shown at `time` as an image file, PNG, JPEG or BMP by
/// the extension of `path`. Answers the size it was written at.
///
/// The frame the editor stands on, at the size the recording is shown at:
/// every sample across, in square pixels, so 1440x1080 at 16:9 is saved
/// 1920x1080 and a DVD's 720x480 at 16:9 is 853x480. An interlaced picture
/// is saved from its top field, as a cover is (see [`poster_at`]) and for the
/// same reason: two fields a sixtieth of a second apart woven into one still
/// are a comb along everything that moved.
///
/// The colours are converted with the matrix the recording says it was made
/// with -- or the one its size implies where it says nothing -- and not with
/// the BT.601 one a JPEG decoder assumes: an HD picture saved without that
/// comes out with its reds orange and its greens yellow. Nothing more than
/// that is done to them. A PQ or HLG recording is saved as its samples, which
/// on an SDR screen look as flat as they do in the editor.
pub fn save_still(src: &Source, time: f64, path: &std::path::Path) -> Result<(u32, u32)> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let (codec, format) = match ext.as_str() {
        "png" => (ff::codec::Id::PNG, ff::format::Pixel::RGB24),
        "jpg" | "jpeg" => (ff::codec::Id::MJPEG, ff::format::Pixel::YUVJ444P),
        "bmp" => (ff::codec::Id::BMP, ff::format::Pixel::BGR24),
        _ => return Err(anyhow!("a frame is saved as .png, .jpg or .bmp, not {}", path.display())),
    };
    // A whole coded picture, not the woven frame, where the recording repeats
    // fields: two frames in five of 2:3 pulldown are woven from two pictures,
    // a comb on a frame that does not say it is interlaced, and was saved as
    // one. See [`still_picture`].
    let picture = still_picture(src, time)?;
    let sar = credible_sar(src.video.sample_aspect_ratio);
    // Every sample across, and as many lines as that width shows. Settled
    // on the frame, before a field is taken out of it: the field has every
    // sample across too, and stands for a frame twice its height. Not held
    // to even numbers, as a video frame is: nothing here is subsampled, and
    // a 16:9 DVD rounded down to 852 came out 478 lines tall.
    let width = ((picture.width() as f64 * sar.max(1.0)).round() as u32).max(16);
    let (picture, sar) = if picture.is_interlaced() && picture.height() >= 32 {
        (top_field(&picture), sar / 2.0)
    } else {
        (picture, sar)
    };
    let height = (((width as f64 * picture.height() as f64) / (picture.width() as f64 * sar)).round()
        as u32)
        .max(16);
    let rgb = to_rgb(&picture, width, height)?;
    let image = if format == ff::format::Pixel::RGB24 {
        rgb
    } else {
        // From RGB, which is the one step that knows the matrix; RGB to a
        // JPEG's YUV is BT.601 full range, which is what a JPEG is read as.
        let mut out = ff::frame::Video::new(format, width, height);
        let mut scaler = ff::software::scaling::Context::get(
            ff::format::Pixel::RGB24,
            width,
            height,
            format,
            width,
            height,
            ff::software::scaling::Flags::POINT,
        )?;
        scaler.run(&rgb, &mut out)?;
        out
    };
    let bytes = encode_still(&image, codec)?;
    std::fs::write(path, bytes).map_err(|e| anyhow!("cannot write {}: {e}", path.display()))?;
    Ok((width, height))
}

/// The picture [`save_still`] saves for the frame shown at `time`.
///
/// Where the recording repeats fields, the frame the editor shows (see
/// [`crate::weave`]) can be the last field of one coded picture and the first
/// of the next. The still is then the picture that frame's top field comes
/// from -- the field an interlaced picture is saved from -- whole: every
/// line of it a line of the same instant, and half of them the lines on the
/// screen. The coded picture *nearest* the instant was the other one wherever
/// the frame begins on a top field, and between the two the pick turned on a
/// tick of the 90 kHz clock, a field being 1501.5 of them. Laid out field by
/// field as the weave lays them out, from the same first access point.
///
/// Every other recording: the picture at `time`, as the editor shows it.
fn still_picture(src: &Source, time: f64) -> Result<ff::frame::Video> {
    if !crate::weave::woven(src) {
        return picture_in(src, time, false).map(|(_, p)| p);
    }
    let fd = src.video.frame_duration();
    let field = fd / 2.0;
    // Where the frame shown at `time` begins, which is the frame
    // `picture_in` answers for it woven.
    let shown = crate::weave::on_frame(src, time);
    let from = entry_before(&src.points, shown);
    // Counted in fields from `head`, as the weave counts them. Bounded well
    // inside i64, as there.
    let slot = |t: f64, head: f64| {
        const FAR: f64 = (1i64 << 52) as f64;
        ((t - head) / field).round().clamp(-FAR, FAR) as i64
    };
    for (attempt, margin) in [0.0, src.seek_margin].into_iter().enumerate() {
        let mut head = src.points.first().map(|p| p.time);
        // The frame's two fields, by the picture each is taken from and
        // whether it is that picture's top field.
        let mut slots: [Option<(Rc<ff::frame::Video>, bool)>; 2] = [None, None];
        let began = walk(src, from, margin, false, false, Cores::One, false, None, |t, frame| {
            let head = *head.get_or_insert(t);
            let first = slot(shown, head);
            let at = slot(t, head);
            if at > first + 1 {
                return false;
            }
            let (fields, top_first) = unsafe {
                let f = &*frame.as_ptr();
                (
                    2 + i64::from(f.repeat_pict.clamp(0, 6)),
                    f.flags & ff::ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST != 0,
                )
            };
            let mut picture: Option<Rc<ff::frame::Video>> = None;
            for i in 0..fields {
                let slot = at + i - first;
                if slot == 0 || slot == 1 {
                    let p = picture.get_or_insert_with(|| Rc::new(frame.clone())).clone();
                    slots[slot as usize] = Some((p, top_first ^ (i % 2 == 1)));
                }
            }
            true
        })?;
        // A walk that began past the frame's first field -- the seek landed
        // late, or an open GOP's leading pictures did not decode -- has only
        // its second, which is not the one wanted. Again from further back.
        let late = match (began, head) {
            (Some(b), Some(h)) => slot(b, h) > slot(shown, h),
            _ => true,
        };
        if late && attempt == 0 {
            continue;
        }
        let picked = match slots {
            // Two pictures, a field of each: the top one.
            [Some((a, a_top)), Some((b, b_top))] if !Rc::ptr_eq(&a, &b) && a_top != b_top => {
                Some(if a_top { a } else { b })
            }
            // One picture, or fields that stopped alternating, which the
            // weave shows as the first.
            [Some((a, _)), _] | [None, Some((a, _))] => Some(a),
            [None, None] => None,
        };
        if let Some(p) = picked {
            return Ok(Rc::try_unwrap(p).unwrap_or_else(|p| (*p).clone()));
        }
    }
    // Nothing landed on that frame: as the editor would answer, unwoven.
    picture_in(src, time, false).map(|(_, p)| p)
}

/// `picture` as RGB at `width` x `height`, through the matrix it was made
/// with.
fn to_rgb(picture: &ff::frame::Video, width: u32, height: u32) -> Result<ff::frame::Video> {
    use ff::ffi;
    let full = crate::blend::full_range_frame(picture);
    // What the picture says it is, or by its size where it says nothing: HD
    // and up is BT.709, anything smaller BT.601.
    let space = match picture.color_space() {
        ff::color::Space::BT709 => ffi::SWS_CS_ITU709,
        ff::color::Space::BT2020NCL | ff::color::Space::BT2020CL => ffi::SWS_CS_BT2020,
        ff::color::Space::FCC => ffi::SWS_CS_FCC,
        ff::color::Space::SMPTE240M => ffi::SWS_CS_SMPTE240M,
        ff::color::Space::BT470BG | ff::color::Space::SMPTE170M => ffi::SWS_CS_ITU601,
        _ if picture.height() >= 720 => ffi::SWS_CS_ITU709,
        _ => ffi::SWS_CS_ITU601,
    };
    let mut out = ff::frame::Video::new(ff::format::Pixel::RGB24, width, height);
    unsafe {
        let c = ffi::sws_getContext(
            picture.width() as i32,
            picture.height() as i32,
            ffi::AVPixelFormat::from(picture.format()),
            width as i32,
            height as i32,
            ffi::AVPixelFormat::AV_PIX_FMT_RGB24,
            (ff::software::scaling::Flags::LANCZOS
                | ff::software::scaling::Flags::FULL_CHR_H_INT
                | ff::software::scaling::Flags::ACCURATE_RND)
                .bits(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        if c.is_null() {
            return Err(anyhow!(
                "cannot convert {:?} {}x{} to RGB",
                picture.format(),
                picture.width(),
                picture.height()
            ));
        }
        ffi::sws_setColorspaceDetails(
            c,
            ffi::sws_getCoefficients(space),
            i32::from(full),
            ffi::sws_getCoefficients(ffi::SWS_CS_DEFAULT),
            1,
            0,
            1 << 16,
            1 << 16,
        );
        let r = ffi::sws_scale(
            c,
            (*picture.as_ptr()).data.as_ptr() as *const *const u8,
            (*picture.as_ptr()).linesize.as_ptr(),
            0,
            picture.height() as i32,
            (*out.as_mut_ptr()).data.as_ptr(),
            (*out.as_mut_ptr()).linesize.as_ptr(),
        );
        ffi::sws_freeContext(c);
        if r <= 0 {
            return Err(anyhow!("the picture could not be converted to RGB"));
        }
    }
    Ok(out)
}

/// One picture through an image encoder.
fn encode_still(picture: &ff::frame::Video, id: ff::codec::Id) -> Result<Vec<u8>> {
    let codec = ff::encoder::find(id).ok_or_else(|| anyhow!("no {id:?} encoder"))?;
    let mut enc = ff::codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()?;
    enc.set_width(picture.width());
    enc.set_height(picture.height());
    enc.set_format(picture.format());
    enc.set_time_base(ff::Rational::new(1, 25));
    let jpeg = id == ff::codec::Id::MJPEG;
    if jpeg {
        // As good as a JPEG gets before it is only bigger.
        unsafe {
            (*enc.as_mut_ptr()).flags |= ff::ffi::AV_CODEC_FLAG_QSCALE as i32;
            (*enc.as_mut_ptr()).global_quality = ff::ffi::FF_QP2LAMBDA * 2;
        }
        enc.set_color_range(ff::color::Range::JPEG);
    }
    let mut enc = enc.open_as(codec)?;
    let mut frame = picture.clone();
    if jpeg {
        frame.set_color_range(ff::color::Range::JPEG);
    }
    frame.set_pts(Some(0));
    enc.send_frame(&frame)?;
    enc.send_eof()?;
    let mut packet = ff::Packet::empty();
    let mut out = Vec::new();
    while enc.receive_packet(&mut packet).is_ok() {
        out.extend_from_slice(packet.data().unwrap_or(&[]));
        packet = ff::Packet::empty();
    }
    if out.is_empty() {
        return Err(anyhow!("the {id:?} encoder produced nothing"));
    }
    Ok(out)
}

/// Put a freshly opened demuxer at the access point `from`.
///
/// Shared by everything that reads a stretch of a recording rather than the
/// whole of it, so that they all land in the same place: the two of them
/// disagreeing is a run of pictures that is a GOP out on one path and not on
/// the other.
fn place(ictx: &mut crate::input::Demux, src: &Source, from: f64, margin: f64) -> Result<()> {
    // The index knows the byte `from` begins at, so on the first attempt
    // there is nothing to approximate and no margin to spend. The timestamp
    // seek below is what is left for the containers and indexes that cannot
    // say -- and for the second attempt, which only happens now when the
    // first somehow still landed late.
    if margin == 0.0 && crate::index::seek_to_entry(ictx, src, from).is_some() {
        return Ok(());
    }
    let landing = (from - margin).max(0.0);
    // Asking for the beginning has to mean the beginning, exactly as in
    // `cut::seek_to`: aiming at the container's own start time lands
    // *past* the file's first entry point, and that is the one place the
    // back-off above has nothing earlier to fall back to. A recording
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
    Ok(())
}

/// How many cores [`walk`] decodes on.
///
/// One, for a walk that stops as soon as it has the picture it came for:
/// frame threading holds pictures back until its pipeline fills, so every
/// core means reading and decoding further past the answer before it comes
/// out.
///
/// Every core for playback, which has to keep up with the recording and is
/// going to read on anyway. On one core a UHD disc's 4K HEVC decoded at 21
/// pictures a second against the 23.976 it plays at -- 88% of real time, so
/// the picture fell behind the sound and 再生 went by in lurches -- and a
/// 1080p Blu-ray at 45, which is too little headroom to survive the
/// thumbnail pass running beside it. See `examples/playrate.rs`.
#[derive(Clone, Copy)]
enum Cores {
    One,
    All,
    /// Every core that gives the same pictures every time: see
    /// [`steady_decoder`].
    Steady,
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
/// and nothing else. `frames` weaves a recording that repeats fields into
/// the frames a screen shows; see [`crate::weave`]. `visit` returns false to
/// stop. `wall` is the byte the pictures stop at -- the seam after the
/// stretch being read, on a recorder's clip -- where they stop at one.
#[allow(clippy::too_many_arguments)]
fn walk(
    src: &Source,
    from: f64,
    margin: f64,
    keys: bool,
    frames: bool,
    cores: Cores,
    whole: bool,
    wall: Option<u64>,
    mut visit: impl FnMut(f64, &ff::frame::Video) -> bool,
) -> Result<Option<f64>> {
    let mut ictx = crate::input::demux(&src.input.url)?;
    let idx = src.video.stream_index;
    let in_tb = src.video.time_base;
    place(&mut ictx, src, from, margin)?;
    // Nothing but the pictures is read here, and on some recordings a stream
    // left switched on costs the pictures themselves. See
    // [`crate::input::keep_only`], and note that it goes after the seek.
    crate::input::keep_only(&mut ictx, &[idx]);

    let params = ictx
        .stream(idx)
        .ok_or_else(|| anyhow!("stream {idx} vanished"))?
        .parameters();
    let mut decoder = match cores {
        Cores::One => ff::codec::context::Context::from_parameters(params)?
            .decoder()
            .video()?,
        Cores::All => crate::video_decoder(params)?,
        Cores::Steady => steady_decoder(params)?,
    };

    let mut frame = ff::frame::Video::empty();
    let mut first = None;
    let mut stopped = false;
    let mut entries = crate::EntryPictures::new(&src.video);
    // A recording that repeats fields is shown frame by frame, as a screen
    // shows it, rather than picture by picture. Not for a walk over the entry
    // pictures alone: they are asked about as pictures, and the fields
    // between them are not decoded to be woven with. See [`crate::weave`].
    let mut weave =
        (frames && !keys && crate::weave::woven(src)).then(|| crate::weave::Weave::new(src));
    // Hand one decoded picture over, woven or not; false once the visitor
    // has had enough.
    let mut offer = |t: f64,
                     frame: &mut ff::frame::Video,
                     weave: &mut Option<crate::weave::Weave>|
     -> Result<bool> {
        let Some(w) = weave.as_mut() else {
            return Ok(visit(t, frame));
        };
        // Taken rather than copied: the weave holds on to it for the frame
        // after, and the decoder is handed an empty one to fill next.
        let picture = std::mem::replace(frame, ff::frame::Video::empty());
        for (at, shown) in w.push(t, picture)? {
            if !visit(at, &shown) {
                return Ok(false);
            }
        }
        Ok(true)
    };
    // Where the pictures have got to, in bytes: see `wall`. A second field
    // says nowhere and stands where its first did.
    let mut read_at: Option<u64> = None;
    // **A recorder's seam is read across as the cut reads it.** The decoder
    // is drained of the stretch before it, and nothing of the stretch after
    // it that is stamped before the stretch begins is handed on -- leading
    // pictures of a GOP the recorder cut, which the cut never writes (see
    // [`crate::cut`]'s `stretch_bytes`). Fed straight on, the decoder threw
    // away the last half second of the stretch before: a picture asked for
    // there, the film strip's cells and playback all answered with one up
    // to a fifth of a second off, and a frame saved there or a cover taken
    // there was another picture. `next_seam` is the first seam not yet
    // passed; `since`, where the stretch being read begins.
    let mut next_seam = 0usize;
    let mut since: Option<f64> = None;
    // Whether the decoder holds anything a drain would hand back.
    let mut fed = false;
    let quarter = src.video.frame_duration() / 4.0;
    // The other end of a stretch, read the same way: where the cut stops the
    // range before seam `k` (see `plan`'s `at_the_seams`, the earlier of the
    // seam's two instants). A recorder's stretch can run a picture or two
    // past the time its sequence table gives it -- 0.05 s on one recorder
    // title, a third of a second on another -- and the plan ends the range
    // before them; shown, the editor stood on a picture past the range's end.
    // A picture that *begins* before that instant is the range's, as the cut
    // writes it (to a thousandth of a frame, as its re-encode compares): a
    // stretch whose table end is not on its pictures has its last one begin
    // a fraction of a frame before it, and dropped at a quarter of a frame
    // the check counted a picture fewer than the cut wrote -- a recorder
    // title's range ending at such a seam failed with 120 written, 119
    // expected.
    let tol = src.video.frame_duration() * 1e-3;
    let stops = |k: usize| src.joins.get(k).map(|j| j.ends.min(j.time));
    let outside =
        |t: f64, since: Option<f64>, stop: Option<f64>| off_stretch(t, since, stop, quarter, tol);
    let mut packets = ictx.read_packets();
    'outer: for (stream, packet) in packets.by_ref() {
        if stream.index() != idx {
            continue;
        }
        if packet.position() >= 0 {
            read_at = Some(packet.position() as u64);
        }
        // Past the wall is the next stretch, whose pictures are not this
        // one's whatever their times say; the drain below hands back what
        // the decoder still holds, as [`crate::cut`]'s re-encode does there.
        if read_at.zip(wall).is_some_and(|(at, w)| at >= w) {
            break;
        }
        if let Some(at) = read_at {
            let was = since;
            let ended = stops(next_seam);
            let mut crossed = false;
            while let Some(j) = src.joins.get(next_seam).filter(|j| at >= j.at) {
                since = Some(j.time);
                next_seam += 1;
                crossed = true;
            }
            if crossed && fed {
                fed = false;
                let _ = decoder.send_eof();
                while decoder.receive_frame(&mut frame).is_ok() {
                    let Some(pts) = frame.pts() else { continue };
                    let t = pts as f64 * in_tb - src.start_time;
                    if outside(t, was, ended) {
                        continue;
                    }
                    if first.is_none() {
                        first = Some(t);
                    }
                    if !offer(t, &mut frame, &mut weave)? {
                        stopped = true;
                        break 'outer;
                    }
                }
                decoder.flush();
                entries.broke();
            }
        }
        // Never handed over rather than decoded and dropped -- and filtered
        // here rather than with `skip_frame`, which is per *picture* and so
        // takes the P bottom field off a field-coded entry point; see the
        // note in `thumbs::build`.
        //
        // Which packets an entry picture is made of is not always the one
        // marked: PAFF codes it as two, and the decoder gives nothing back
        // until both have gone in. See [`crate::EntryPictures`].
        let step = if keys {
            entries.step(&packet)
        } else {
            crate::Step::Whole
        };
        if step == crate::Step::Skip {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            entries.broke();
            continue;
        }
        fed = true;
        // Half a picture. Nothing can come out yet, and the drain below would
        // throw the half away rather than wait for its partner.
        if step == crate::Step::Half {
            continue;
        }
        // An entry picture decodes on its own, which is the whole reason the
        // packets between them may be skipped -- so each one is drained on
        // its own too. A decoder handed a *subset* of a stream's packets
        // cannot order what comes out of it: it orders by the picture order
        // count, and the counts of pictures that are not consecutive say
        // nothing about which comes first. On one disc that handed the film
        // strip an entry picture half a second late, past the end of the run
        // being drawn, and the run stopped there with its remaining cells
        // empty. Drained one at a time, what comes out is what went in, in
        // the order the file holds them.
        if keys {
            let _ = decoder.send_eof();
        }
        while decoder.receive_frame(&mut frame).is_ok() {
            let Some(pts) = frame.pts() else { continue };
            let t = pts as f64 * in_tb - src.start_time;
            if outside(t, since, stops(next_seam)) {
                continue;
            }
            if first.is_none() {
                first = Some(t);
            }
            if !offer(t, &mut frame, &mut weave)? {
                stopped = true;
                break 'outer;
            }
        }
        if keys {
            decoder.flush();
            fed = false;
        }
    }
    // A read that gave up part way, for the one caller that has to know
    // (`whole`): the pictures it was counting stopped there, not at the end
    // of what was asked. The others show what they could, as the detections
    // do.
    let unread = packets.finished().err();
    drop(packets);
    if whole && !stopped {
        if let Some(e) = unread {
            return Err(e);
        }
    }
    if !stopped {
        // Whatever is still held for reordering, so a request at the very end
        // of the file is answered rather than falling off it.
        let _ = decoder.send_eof();
        while decoder.receive_frame(&mut frame).is_ok() {
            // As the loop above does: a picture with no time is passed over,
            // not given the time of the entry point it was decoded from.
            let Some(t) = frame.pts().map(|p| p as f64 * in_tb - src.start_time) else {
                continue;
            };
            if outside(t, since, stops(next_seam)) {
                continue;
            }
            if first.is_none() {
                first = Some(t);
            }
            if !offer(t, &mut frame, &mut weave)? {
                stopped = true;
                break;
            }
        }
        if !stopped {
            if let Some(w) = weave.as_mut() {
                for (at, shown) in w.finish() {
                    if !visit(at, &shown) {
                        break;
                    }
                }
            }
        }
    }
    Ok(first)
}

/// Whether a picture presented at `t`, read inside the stretch that begins at
/// `since` and stops at `stop`, is one the cut never writes: a leading
/// picture of the stretch's first GOP stamped before it begins, or one past
/// where the plan ends the range before the seam after it. See [`walk`],
/// whose rule this is; [`Stretches`] asks it for the readers that take entry
/// pictures alone.
fn off_stretch(t: f64, since: Option<f64>, stop: Option<f64>, quarter: f64, tol: f64) -> bool {
    since.is_some_and(|s| t < s - quarter) || stop.is_some_and(|e| t >= e - tol)
}

/// Which stretch of a recorder's joined clip a read of its pictures is in,
/// told by the bytes as [`walk`] tells it, for the readers that pick the
/// entry pictures out of the packets without decoding the rest: the film
/// strip's entry runs ([`entry_packets`]) and the thumbnail pass
/// ([`crate::thumbs::build_with_sound`]).
///
/// **They pass over the same pictures [`walk`] passes over.** The stretches
/// of a recorder's clip overlap on the joined clock, and the last entry
/// picture of the stretch before a seam can present past the instant the
/// plan ends the range there -- a picture the cut never writes, and one the
/// editor no longer stands on. Taken by the strip, a cell at that instant
/// showed the end of the previous recording inside the next one, and the
/// thumbnail track held it in place of the next stretch's first entry
/// picture. Nothing changes on a recording without seams.
pub(crate) struct Stretches<'a> {
    joins: &'a [crate::restamp::Seam],
    next: usize,
    since: Option<f64>,
    quarter: f64,
    tol: f64,
}

impl<'a> Stretches<'a> {
    pub(crate) fn new(src: &'a Source) -> Self {
        let fd = src.video.frame_duration();
        Self::of(&src.joins, fd)
    }

    fn of(joins: &'a [crate::restamp::Seam], fd: f64) -> Self {
        Self {
            joins,
            next: 0,
            since: None,
            quarter: fd / 4.0,
            tol: fd * 1e-3,
        }
    }

    /// A packet of the pictures at byte `pos` has been read; a negative one
    /// says nowhere and leaves the read where the last one put it.
    pub(crate) fn reach(&mut self, pos: isize) {
        if pos < 0 {
            return;
        }
        while let Some(j) = self.joins.get(self.next).filter(|j| pos as u64 >= j.at) {
            self.since = Some(j.time);
            self.next += 1;
        }
    }

    /// Whether a picture at `t`, read where the read now is, is outside the
    /// stretch it was read in.
    pub(crate) fn outside(&self, t: f64) -> bool {
        let stop = self.joins.get(self.next).map(|j| j.ends.min(j.time));
        off_stretch(t, self.since, stop, self.quarter, self.tol)
    }
}

/// Every coded picture presented in `[from, to)`, in presentation order.
///
/// For [`crate::verify`], which lines a cut up against what it was cut
/// from. Pictures and not woven frames: a cut carries a recording's pictures
/// as they were coded, repeat flags and all, and the output is decoded the
/// same way, so the two sides count the same things.
///
/// Begins at the access point before `from`, as everything that reads a
/// stretch does, and starts again a margin earlier where the landing was
/// late -- found out on the first picture, before any has been handed on,
/// so nothing is handed on twice. `visit` returns false to stop.
///
/// `head` is where the re-encode that opens the range ends, when one does.
/// The picture already up at `from` is handed on too where the cut writes
/// it: see below.
pub(crate) fn pictures_in(
    src: &Source,
    from: f64,
    to: f64,
    head: Option<f64>,
    mut visit: impl FnMut(f64, &ff::frame::Video) -> bool,
) -> Result<()> {
    crate::init()?;
    let fd = src.video.frame_duration();
    let entry = entry_before(&src.points, from);
    // The same slack the re-encode in `cut` allows at a range's ends: enough
    // to absorb a timestamp's rounding and no more, because real streams put
    // their pictures at an arbitrary phase rather than on multiples of a
    // frame. Not a slack of its own: a picture 0.06 ms short of a range's end
    // -- a phone's recording, an end set at 133.220 -- was one the cut wrote
    // and a fixed 0.1 ms here left out, and the count was one over.
    let eps = fd * 1e-3;
    // **The picture already up when the range opens.** The re-encode in
    // `cut` writes it at the range's start where nothing of the range's own
    // begins within a frame of it -- unless its own repeat carries it to the
    // next picture -- and always where nothing at all begins inside the
    // opening re-encode. On a recording that mixes pictures shown for three
    // fields with ones shown for two, a range lands inside one of those
    // often enough; left out here, every pair after it was one out.
    // `next` is the first picture at or after `from`, if there is one.
    let up_too = |was: f64, held: &ff::frame::Video, next: Option<f64>| {
        let Some(t) = next else { return head.is_some() };
        if head.is_some_and(|end| t >= end - eps) {
            return true;
        }
        let shown_for = 2 + unsafe { (*held.as_ptr()).repeat_pict.max(0) };
        let own_end = was + f64::from(shown_for) * fd / 2.0;
        t - from >= fd && own_end < t - fd / 4.0
    };
    // **The stretch `to` lies in, and not the one after it.** On a
    // recorder's clip the next stretch can open on pictures stamped before
    // its seam -- leading pictures of a GOP the recorder cut, whose
    // references are gone -- and on the joined clock they fall inside a
    // range that ends at the seam. The cut stops at the seam's byte and
    // never writes them ([`crate::cut`]'s `stretch_bytes`), so the count
    // stops there too: read on, it took one of them for a picture the cut
    // had lost.
    let wall = src.joins.iter().find(|j| j.time >= to - eps).map(|j| j.at);
    for (attempt, margin) in [0.0, src.seek_margin].into_iter().enumerate() {
        let mut late = false;
        let mut seen = false;
        // The last picture before `from`, until the first one after it.
        let mut before: Option<(f64, ff::frame::Video)> = None;
        let mut opened = false;
        let mut going = true;
        walk(src, entry, margin, false, false, Cores::Steady, true, wall, |t, frame| {
            if !seen {
                seen = true;
                // Only a walk that has not yet handed anything on can be
                // started again.
                if attempt == 0 && landed_late(Some(t), entry, fd / 2.0) {
                    late = true;
                    return false;
                }
            }
            if t < from - eps {
                before = Some((t, frame.clone()));
                return true;
            }
            if !opened {
                opened = true;
                if let Some((was, held)) = before.take() {
                    if up_too(was, &held, Some(t)) && !visit(was, &held) {
                        going = false;
                        return false;
                    }
                }
            }
            if t >= to - eps {
                return false;
            }
            going = visit(t, frame);
            going
        })?;
        if late {
            continue;
        }
        // The file ended before anything began in the range.
        if !opened && going {
            if let Some((was, held)) = before.take() {
                if up_too(was, &held, None) {
                    visit(was, &held);
                }
            }
        }
        break;
    }
    Ok(())
}

/// A decoder whose pictures are the same every time it is run.
///
/// For [`crate::verify`], which holds two decodes of the same bits to being
/// the same. Threaded, the codecs that conceal damage -- MPEG-2, H.264, VC-1
/// -- conceal it differently from run to run, frame threads and slice threads
/// alike, and a picture after the damage that nothing flags came out
/// different on one side: the same damaged cut checked five times failed
/// two or three times, and on one thread never. So those decode on one
/// thread (an MPEG-2 broadcast still checks at about 300 pictures a second a
/// side). The codecs without concealment keep every core.
pub(crate) fn steady_decoder(params: ff::codec::Parameters) -> Result<ff::decoder::Video> {
    let conceals = matches!(
        params.id(),
        ff::codec::Id::MPEG1VIDEO
            | ff::codec::Id::MPEG2VIDEO
            | ff::codec::Id::H264
            | ff::codec::Id::VC1
            | ff::codec::Id::WMV3
            | ff::codec::Id::MPEG4
            | ff::codec::Id::H263
    );
    let mut ctx = ff::codec::context::Context::from_parameters(params)?;
    unsafe {
        let c = ctx.as_mut_ptr();
        (*c).thread_count = if conceals { 1 } else { 0 };
    }
    Ok(ctx.decoder().video()?)
}

/// Did decoding begin late enough to have missed the picture wanted?
fn landed_late(first: Option<f64>, wanted: f64, slack: f64) -> bool {
    !matches!(first, Some(f) if f <= wanted + slack)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pixel_aspect_ratio_is_held_to_one_a_picture_can_have() {
        // Every real one passes through, a field's halved one included.
        for sar in [1.0, 4.0 / 3.0, 8.0 / 9.0, 32.0 / 27.0, 8.0 / 9.0 / 2.0] {
            assert_eq!(credible_sar(sar), sar);
        }
        // A crafted 1:65535 asked for a still 65535 times its height.
        assert_eq!(credible_sar(1.0 / 65535.0), 0.125);
        assert_eq!(credible_sar(65535.0), 8.0);
        assert_eq!(credible_sar(f64::NAN), 1.0);
    }

    /// Each slot takes the nearest picture inside its window and no other;
    /// two slots that settle on the same picture keep it once, in the
    /// earlier slot, and the slots stay where they were put.
    #[test]
    fn slots_take_the_nearest_and_never_shift() {
        let fd = 1.0 / 30.0;
        let wanted = [1.0, 1.01, 2.0, 9.0];
        let mut slots: Slots<'_, u32> = Slots::new(&wanted, 0.5);
        for (n, t) in [0.4, 0.9, 1.02, 1.6, 2.1, 2.05].into_iter().enumerate() {
            slots.offer(t, "I", || Ok(n as u32)).unwrap();
        }
        let got = slots.finish(fd, |n| Ok(vec![n as u8])).unwrap();
        assert_eq!(got.len(), 4);
        let at: Vec<Option<f64>> = got.iter().map(|s| s.as_ref().map(|s| s.time)).collect();
        assert_eq!(at, vec![Some(1.02), None, Some(2.05), None]);
        assert_eq!(got[0].as_ref().unwrap().jpeg, vec![2]);
    }

    /// A recorder's clip with two seams: the stretches overlap on the joined
    /// clock, and the second's table end is past its pictures. Read by the
    /// bytes, a picture belongs to the stretch it was read in, and one past
    /// where the plan ends the range before the next seam -- or a leading
    /// picture stamped before its own stretch begins -- is outside it.
    #[test]
    fn stretches_are_told_by_their_bytes() {
        use crate::restamp::Seam;
        let fd = 1.0 / 30.0;
        let joins = [
            // Stretch 1 begins at 100.0, though stretch 0 runs on to 100.3.
            Seam { at: 1000, time: 100.0, ends: 100.0 },
            // Stretch 1's pictures end at 150.0 (mended); stretch 2 at 160.0.
            Seam { at: 2000, time: 160.0, ends: 150.0 },
        ];
        let mut s = Stretches::of(&joins, fd);
        // Nothing read yet: the first stretch, which begins with the clip.
        s.reach(-1);
        assert!(!s.outside(0.0));
        s.reach(500);
        assert!(!s.outside(99.9));
        // Past the seam's instant on the joined clock, still stretch 0's.
        assert!(s.outside(100.3));
        // A hair under the instant is the range's, as the cut writes it.
        assert!(!s.outside(100.0 - fd * 0.5));
        assert!(s.outside(100.0 - fd * 1e-4));
        // A packet that says nowhere leaves the read where it was.
        s.reach(-1);
        assert!(s.outside(100.3));
        // Into stretch 1 by the bytes: its own pictures are in, a leading
        // one stamped well before it begins is not.
        s.reach(1000);
        assert!(!s.outside(100.3));
        assert!(!s.outside(100.0 - fd / 8.0));
        assert!(s.outside(99.8));
        // Its end is where its pictures end, not where its table says.
        assert!(!s.outside(149.9));
        assert!(s.outside(150.0));
        assert!(s.outside(155.0));
        // Two seams in one step land in the last of them.
        let mut s = Stretches::of(&joins, fd);
        s.reach(5000);
        assert!(!s.outside(170.0));
        assert!(s.outside(155.0));
        // No seams, nothing is outside.
        let s = Stretches::of(&[], fd);
        assert!(!s.outside(-5.0) && !s.outside(1e9));
    }

    #[test]
    fn the_entry_before_and_a_late_landing() {
        let points: Vec<AccessPoint> = [0.5, 1.0, 1.5]
            .into_iter()
            .map(|time| AccessPoint {
                time,
                lead_start: time,
                lead_indices: Vec::new(),
                droppable: true,
                pos: -1,
                measured: true,
            })
            .collect();
        assert_eq!(entry_before(&points, 1.2), 1.0);
        assert_eq!(entry_before(&points, 1.0), 1.0);
        // Before the first, or not a number: the front.
        assert_eq!(entry_before(&points, 0.1), 0.0);
        assert_eq!(entry_before(&points, f64::NAN), 0.0);
        assert!(on_point(&points, 1.49, 0.02));
        assert!(!on_point(&points, 1.2, 0.02));
        assert!(!on_point(&[], 0.0, 0.02));
        assert!(landed_late(None, 1.0, 0.1));
        assert!(landed_late(Some(1.2), 1.0, 0.1));
        assert!(!landed_late(Some(1.05), 1.0, 0.1));
    }
}
