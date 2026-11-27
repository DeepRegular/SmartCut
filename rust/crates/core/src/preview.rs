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
    //
    // On an entry point to within half the closest two pictures can come,
    // not half the average: on a screen recording averaging 9 a second that
    // was 55 ms, so a cell standing on the picture 33 ms after an entry
    // point -- a single cell is a run at the strip's wider settings -- was
    // given the entry picture, which is another one. See
    // [`crate::VideoInfo::least_gap`]; it is a frame on every recording whose
    // pictures come at a rate.
    let keys = times
        .iter()
        .all(|&t| on_point(&src.points, t, src.video.least_gap() / 2.0));

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
    fn finish(mut self, image: impl Fn(P) -> Result<Vec<u8>>) -> Result<Vec<Option<Shot>>> {
        // Two slots can round to the same picture -- the strip's spacing need
        // not be a whole number of picture intervals, and under pulldown it
        // never is. The later slot gives it up rather than repeating it. The
        // same picture, which is the same instant: not two within half the
        // closest two pictures come, which a recording's last picture can be
        // of the one before it where the measure of that closest is taken
        // without it -- a phone's last picture a fraction of a millisecond
        // after the one before, whose cell then stayed empty though the
        // picture on screen there was that one.
        for i in (1..self.got.len()).rev() {
            let dup = match (&self.got[i], &self.got[i - 1]) {
                (Some(a), Some(b)) => a.0 == b.0,
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
        let began = walk(src, from, margin, false, true, Cores::One, false, None, true, |t, frame| {
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
    got.finish(|frame| encode_jpeg(&frame, sar, width))
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
    got.finish(Ok)
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
    let mut landed = place(&mut ictx, src, from, margin)?;
    let idx = src.video.stream_index;
    // After the seek, for the reason [`crate::input::keep_only`] gives.
    crate::input::keep_only(&mut ictx, &[idx]);
    let params = ictx
        .stream(idx)
        .ok_or_else(|| anyhow!("stream {idx} vanished"))?
        .parameters();
    let mut entries = crate::EntryPictures::of(src);
    // Each entry picture may go to any decoder of the pool: see [`InForce`].
    let mut in_force = InForce::pool(&params, &src.video);
    // And the MPEG-4 Part 2 VOL in force: see [`VolInForce`].
    let mut vols = VolInForce::pool(&params);
    let mut out: Vec<(f64, Vec<ff::Packet>)> = Vec::new();
    let mut first = None;
    let mut pending: Vec<ff::Packet> = Vec::new();
    let mut at: Option<f64> = None;
    let mut stretches = Stretches::new(src);
    // An entry picture whose packet carries no time -- an IDR of an H.264
    // `.mpg` that the index lists, timed as the cut times it -- is given
    // that time, as [`walk`] gives it: passed over, the film strip's cell on
    // it stayed empty. See [`Untimed`].
    let mut untimed = Untimed::knowing(&src.video, &params);
    // A palette recording's palette, for the first entry picture: see
    // [`head_palette`]. The pool hands it on to every decoder.
    let mut palette_owed = head_palette(src, &params);
    // **A ceiling on the read, for a recording whose entry points the packets
    // do not flag**: an AVCHD disc whose map enters on I pictures that are
    // neither IDR nor recovery points (see `walk`) has no packet this takes
    // for an entry picture, and every strip run read on to the end of the
    // title, twice. Past the longest gap between two entry points beyond
    // `until`, the next entry picture -- the one the read stops at below --
    // would already have come on every recording whose packets say which
    // they are, so stopping there changes nothing for them.
    let longest = src
        .points
        .windows(2)
        .map(|w| w[1].time - w[0].time)
        .fold(0.0_f64, f64::max);
    let ceiling = until + longest + 1.0;
    let mut beyond = 0u32;
    let mut strays = Strays::new(src);
    let mut reading = untimed.paired(ictx.read_packets(), idx);
    while let Some((stream, mut packet, doubted)) = strays.read(&mut reading) {
        if stream.index() != idx {
            continue;
        }
        Landed::mend(&mut landed, &mut packet, src);
        untimed.place(&mut packet, doubted);
        // As [`walk`] times it: see [`Strays`].
        strays.mend(src, &mut packet, src.video.time_base, &mut reading);
        // Thirty in a row, so that one damaged stamp far ahead stops nothing.
        if packet
            .dts()
            .or(packet.pts())
            .is_some_and(|d| d as f64 * src.video.time_base - src.start_time > ceiling)
        {
            beyond += 1;
            if beyond >= 30 {
                break;
            }
        } else {
            beyond = 0;
        }
        in_force.see(&packet);
        // A half held from the stretch before has no partner after the seam:
        // dropped, as [`walk`] drops it there. See `thumbs::build_with_sound`.
        if stretches.reach(packet.position()) {
            pending.clear();
            at = None;
            entries.broke();
        }
        let step = entries.step(&packet);
        if step == crate::Step::Skip {
            continue;
        }
        in_force.mend(&mut packet, &mut |at, record| Some(sets_before(src, at, record)));
        vols.mend(&mut packet, src, src.video.time_base);
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
        let mut packets = std::mem::take(&mut pending);
        if keep(when) {
            if let (Some(palette), Some(head)) = (palette_owed.take(), packets.first_mut()) {
                give_palette(head, &palette);
            }
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
    // One picture handed over, paced by the caller; false where it said stop.
    let mut put = |t: f64, frame: &ff::frame::Video| match pace(t) {
        Pace::Stop => false,
        Pace::Skip => true,
        Pace::Show => {
            if let Ok(jpeg) = encode_jpeg(frame, src.video.sample_aspect_ratio, width) {
                show(t, jpeg);
            }
            true
        }
    };
    // **The range opens on the picture the cut opens it on** -- unless the
    // recording is woven into frames (see [`crate::weave`]), which is shown
    // frame by frame and keeps the nearer-of-two rule below. A range the
    // cut copies from an entry point begins on that point's picture, up to
    // half the closest two pictures come before `from`; a range it
    // re-encodes begins on the first picture at `from` or after it, or with
    // the picture already up at `from` where nothing arrives within the
    // closest two pictures come and that one is held past its own length
    // (`still_on_screen` in the cut's re-encode, whose rule this is). The
    // nearer of the two either side was the rule for all of them: a range
    // asked for in seconds between two pictures of a constant-rate
    // recording played a picture of what was cut first, which the cut does
    // not write, and a range opening inside a screen recording's hold of a
    // second or two played nothing of the hold, which the cut writes.
    let woven = crate::weave::woven(src);
    let least = src.video.least_gap();
    let tol = fd * 1e-3;
    // A bound within a coarse clock's ticks of a picture is that picture's,
    // as the cut settles it ([`crate::plan::plan_on`]).
    let slack = crate::cut::on_a_picture_slack(&src.video);
    let copied = (!woven)
        .then(|| {
            crate::plan::plan_range(
                &src.video,
                src.duration,
                &src.points,
                from,
                until,
                &crate::plan::PlanOptions::default(),
            )
        })
        .and_then(|plan| plan.segments.first().cloned())
        .filter(|s| s.kind == crate::plan::SegmentKind::Copy)
        .map(|s| s.start);
    // The first instant a picture of the range's own may begin at.
    let own_from = copied.map_or(from - slack, |c| c.min(from - slack) - tol);
    for (attempt, margin) in [0.0, src.seek_margin].into_iter().enumerate() {
        let mut began_late = true;
        // **A picture that begins before `from` is played only where it is
        // the nearer of the two either side of it**, as `picture_in` picks
        // the one the editor stands on -- on a woven recording; see above
        // for every other. Up to half a frame early was the whole rule, and
        // a frame is the average's: on a screen recording averaging 9 a
        // second whose pictures come 33 ms apart wherever the screen moves,
        // half of it is 55 ms, so playing from a picture began on the one
        // before it -- a picture of what was cut, at every range's head.
        let mut early: Option<(f64, ff::frame::Video)> = None;
        let first = walk(src, entry, margin, false, true, Cores::All, false, None, true, |t, frame| {
            if !woven {
                if t < own_from {
                    // Held by reference, not copied: this is every picture
                    // from the entry point up to the range, and a hold can
                    // reach back to any of them.
                    early = Some((t, held_ref(frame)));
                    return true;
                }
                if let Some((e, held)) = early.take() {
                    // Up from the range's start until `t`, as the cut puts
                    // it: only where `t` is at least the closest two
                    // pictures come after the start, and the picture is
                    // held past its own coded length.
                    let shown_for = 2 + unsafe { (*held.as_ptr()).repeat_pict.max(0) };
                    let own_end = e + f64::from(shown_for) * least / 2.0;
                    // And always on a recording of stills, as the cut keeps
                    // it there ([`crate::cut::still_up_from`]).
                    if copied.is_none()
                        && ((t - from >= least && own_end < t - least / 4.0)
                            || crate::cut::still_up_from(src, from, t).is_some())
                    {
                        began_late = false;
                        if !put(e, &held) {
                            stopped = true;
                            return false;
                        }
                    }
                }
            } else {
                if t < from - fd / 2.0 {
                    return true;
                }
                if t < from - 1e-6 {
                    early = Some((t, frame.clone()));
                    return true;
                }
                if let Some((e, held)) = early.take() {
                    if from - e <= t - from {
                        began_late = false;
                        if !put(e, &held) {
                            stopped = true;
                            return false;
                        }
                    }
                }
            }
            if t >= until - 1e-6 {
                // The end of the stretch, which stops the playing -- unless
                // nothing of it has been played: a seek that landed past the
                // whole of a short range is the late landing below, and is
                // tried again from further back rather than played as nothing.
                stopped = !began_late;
                return false;
            }
            began_late = false;
            if !put(t, frame) {
                stopped = true;
                return false;
            }
            true
        })?;
        // The file ended with nothing after it: it is the nearer -- unless
        // it had gone by its own length before the range opened and the
        // range ends within a frame of that: the sound's last frame past
        // the pictures, where the cut writes no picture
        // (`past_the_pictures` in its re-encode; `pictures_in` counts the
        // same). Played, that sliver showed a picture of what was cut.
        let gone = |e: f64, held: &ff::frame::Video| {
            let stated = unsafe { (*held.as_ptr()).duration };
            let ended = if stated > 0 {
                Some(e + stated as f64 * src.video.time_base)
            } else if !src.video.variable_rate {
                let shown_for = 2 + unsafe { (*held.as_ptr()).repeat_pict.max(0) };
                Some(e + f64::from(shown_for) * fd / 2.0)
            } else {
                None
            };
            !woven && ended.is_some_and(|end| end <= from + tol && until <= end + fd + tol)
        };
        if let Some((e, held)) = early.take() {
            if !stopped && !gone(e, &held) {
                began_late = false;
                stopped = !put(e, &held);
            }
        }
        // Landing past the wanted picture means nothing was played; back off
        // and start again earlier, as the scrubbing paths do.
        if !landed_late(first, from, fd / 2.0) || !began_late || attempt == 1 || stopped {
            break;
        }
    }
    Ok(())
}

/// Another reference to a decoded picture's buffers, which costs nothing
/// where the decoder counts them (every decoder here) -- a copy of every
/// sample only where it does not.
fn held_ref(frame: &ff::frame::Video) -> ff::frame::Video {
    let mut held = ff::frame::Video::empty();
    let r = unsafe { ff::ffi::av_frame_ref(held.as_mut_ptr(), frame.as_ptr()) };
    if r < 0 {
        return frame.clone();
    }
    held
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

/// As [`shot_at`], encoded finer: for a picture that stays up on its own
/// rather than one of a run. See [`JPEG_Q_FINE`].
pub fn still_shot_at(src: &Source, time: f64, width: u32) -> Result<Shot> {
    let (at, picture) = picture_at(src, time)?;
    let kind = kind_of(&picture);
    let sar = src.video.sample_aspect_ratio;
    let native = (picture.width() as f64 * credible_sar(sar)).round() as u32;
    let (jpeg, _, _) = encode_jpeg_within(&picture, sar, width.min(native), JPEG_Q_FINE)?;
    Ok(Shot { jpeg, time: at, kind })
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
        let began = walk(src, from, margin, false, woven, Cores::One, false, None, true, |t, frame| {
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
/// out; where that is past the instant and an entry picture stands a little
/// before it, that one is taken instead (see `glance_now`). The time that
/// comes back is the picture's own, so the frame counter says where the
/// picture really is rather than where it was asked for.
///
/// Costs one open, one seek and one GOP: tens of milliseconds, against the
/// second a gigabyte the walk takes to make an exact answer possible.
pub fn glance_at(spec: &str, at: f64, width: u32) -> Result<Shot> {
    glance_now(spec, Landing::At(at), width)
}

/// Where a glance is to land: a fraction of the way in, for a picture that
/// only has to stand for the recording, or a given instant in rebased
/// seconds, for one that has to stand for a moment in it.
#[derive(Clone, Copy)]
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
    /// How much earlier the second try aims. A transport stream is sought by
    /// decoding time, and an entry picture decodes a few pictures before it
    /// is shown: aimed at its own instant, the seek lands on the pictures
    /// decoded after it and the first entry picture from there is the next
    /// GOP's. A quarter of a second is more than the reordering of any of
    /// the broadcast or disc material measured.
    const BACK: f64 = 0.25;
    /// How much earlier a landing that found no picture standing on its own
    /// is asked again: more than the GOP of any disc or broadcast material.
    const AGAIN: f64 = 2.0;
    let mut g = Glancer::open(spec)?;
    let first = g.at_until(landing, width, None);
    // **Asked for an instant and answered with a picture past it**, once
    // more from a little earlier. Aimed at the first picture of a broadcast
    // recording -- which is where the cut editor's stage opens, before the
    // walk has found any entry points -- the glimpse came back a GOP late on
    // 12 of 19 test recordings (0.949 asked, 1.616 shown), and the frame
    // counter stood on the first frame under the second GOP's picture until
    // the walk landed. The second answer is taken only where it is no later
    // than the instant asked for: a landing at or before its ask is what the
    // window places pictures by. Only here, for the stage's single picture: the
    // film strip's cards (`glance_run`) are left as they were.
    //
    // **Read no further than the instant asked for.** An answer past it is
    // not taken, so the second read stops at the first packet decoded after
    // it rather than going on to the next entry picture: on a pressed disc
    // whose entry pictures stand seconds apart that was the whole of the
    // first read over again, for the same picture (2.2 s a glance became
    // 4.6). And only an entry picture replaces an entry picture: a read
    // stopped short answers with whatever it decoded, which is no better than
    // the first answer and may be a picture that leans on ones it skipped.
    let Landing::At(at) = landing else {
        return first.map(|(shot, _)| shot);
    };
    if let Ok((shot, whole)) = &first {
        if shot.time > at + 1e-3 {
            let back = g.at_until(Landing::At((at - BACK).max(0.0)), width, Some(at + 1e-3));
            if let Ok((back, stood)) = back {
                if back.time < shot.time && back.time <= at + 1e-3 && (stood || !whole) {
                    return Ok(back);
                }
            }
        }
    }
    // **Asked for an instant and answered with nothing that stands on its
    // own**, which is what the last second or so of a transport stream gives:
    // the seek lands past the last entry picture, and from there to the end
    // there is nothing to decode from. Asked for the last frame before the
    // walk (End), the stage kept the picture it had and the status line said
    // the preview failed, on 9 of 16 recordings and disc titles tried, or
    // showed a grey field (a pressed disc's last half second). Once more from
    // further back, for an entry picture before the instant -- and only one
    // that stands on its own.
    if !matches!(first, Ok((_, true))) && at > 0.0 {
        let back = g.at_until(Landing::At((at - AGAIN).max(0.0)), width, Some(at + 1e-3));
        if let Ok((back, true)) = back {
            if back.time <= at + 1e-3 {
                return Ok(back);
            }
        }
    }
    first.map(|(shot, _)| shot)
}

/// Whether a decoded picture stands on its own, which is what a glance
/// answers with: an I picture, or one the decoder flags as a key frame. The
/// second is an interlaced VC-1 disc's entry picture -- an I field and a P
/// field, which libavcodec types P, after the field it decoded last -- and
/// waiting for an I picture there read on to the next all-I picture, up to
/// 10 s past the instant and 3 s a glance on a 1080i clip. Every other decoder
/// flags a key frame only on an I picture or a recovery point.
fn stands(frame: &ff::frame::Video) -> bool {
    kind_of(frame) == "I" || unsafe { (*frame.as_ptr()).flags & ff::ffi::AV_FRAME_FLAG_KEY != 0 }
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
    /// The H.264 sets in force where a glance lands, for a key picture that
    /// carries none (see [`InForce`]), and the recording as a [`Source`] to
    /// ask them of -- opened only once one is needed.
    in_force: InForce,
    lent: Option<Option<Source>>,
    /// Pictures whose packets carry no time; see [`Untimed`].
    untimed: Untimed,
    /// The MPEG-4 Part 2 VOL in force where a glance lands; see
    /// [`VolInForce`].
    vols: VolInForce,
    /// How an H.264 stream's units are framed, for an entry picture with no
    /// key flag (see where the packets are fed); `None` for anything else.
    h264: Option<crate::bitstream::NalFraming>,
    /// A palette recording's palette, for the first picture of each glance:
    /// see [`head_palette`].
    palette: Option<Vec<u8>>,
}

/// The recording a [`Glancer`] reads as a [`Source`], opened only once one
/// is asked of.
fn lend<'a>(lent: &'a mut Option<Option<Source>>, spec: &str) -> Option<&'a Source> {
    lent.get_or_insert_with(|| crate::outline(spec).ok().map(|o| o.into_source())).as_ref()
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
        // The shape the walk's outline gives the recording, and the cut its
        // output: the container's where it states one, which is the only
        // place an AV1 .mp4 or .mkv keeps it (see `outline_of`). Read off
        // the codec alone, an anamorphic AV1's glimpse, strip cells and list
        // picture came out square until the walk landed.
        let sar = unsafe {
            let guessed = ff::ffi::av_guess_sample_aspect_ratio(
                ictx.as_ptr() as *mut _,
                stream.as_ptr() as *mut _,
                std::ptr::null_mut(),
            );
            let s = if guessed.num > 0 && guessed.den > 0 {
                guessed
            } else {
                (*params.as_ptr()).sample_aspect_ratio
            };
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
        let in_force = {
            let extradata = unsafe {
                let p = params.as_ptr();
                if (*p).extradata.is_null() || (*p).extradata_size <= 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts((*p).extradata, (*p).extradata_size as usize)
                }
            };
            let on = params.id() == ff::codec::Id::H264;
            let annex_b = on
                && crate::bitstream::framing_from_extradata("h264", extradata)
                    == crate::bitstream::NalFraming::AnnexB;
            (InForce::of(&params, on), annex_b)
        };
        let h264 = (params.id() == ff::codec::Id::H264).then(|| {
            let extradata = unsafe {
                let p = params.as_ptr();
                if (*p).extradata.is_null() || (*p).extradata_size <= 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts((*p).extradata, (*p).extradata_size as usize)
                }
            };
            crate::bitstream::framing_from_extradata("h264", extradata)
        });
        let palette = palette_of(&input.url, idx, &params);
        let (mut in_force, annex_b) = in_force;
        in_force.given = Some(in_force.record.clone());
        in_force.ask = false;
        let rate = stream.avg_frame_rate();
        let rate = if rate.numerator() > 0 && rate.denominator() > 0 { rate } else { stream.rate() };
        let untimed = Untimed::with(annex_b, Some(&params), f64::from(rate.invert()) / time_base);
        let vols = VolInForce::single(&params);
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
            in_force,
            lent: None,
            untimed,
            vols,
            h264,
            palette,
        })
    }

    /// Seek, decode the first picture that stands on its own, and answer with
    /// it and the instant it turned out to be at.
    fn at(&mut self, landing: Landing, width: u32) -> Result<Shot> {
        self.at_until(landing, width, None).map(|(shot, _)| shot)
    }

    /// As [`Self::at`], reading no packet that is decoded after `until`
    /// (rebased seconds): what the decoder holds by then is all the answer
    /// there is. A picture decoded after an instant is shown after it too, so
    /// nothing at or before `until` is missed by stopping there. Says too
    /// whether the picture stands on its own (see [`stands`]) or is the
    /// answer of last resort.
    fn at_until(
        &mut self,
        landing: Landing,
        width: u32,
        until: Option<f64>,
    ) -> Result<(Shot, bool)> {
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
            in_force,
            lent,
            untimed,
            vols,
            h264,
            palette,
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
        // Told to show an unflagged entry picture by the glance before (see
        // below), and not by this one yet.
        unsafe {
            (*decoder.as_mut_ptr()).flags2 &= !ff::ffi::AV_CODEC_FLAG2_SHOW_ALL;
        }
        let mut palette_owed = palette.as_deref();
        // Whether the read started without a key packet, for want of one.
        let mut unkeyed = false;
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
        in_force.restart();
        untimed.restart();
        vols.restart();

        let mut frame = ff::frame::Video::empty();
        // The picture to answer with, which is the first I picture (or key
        // frame; see [`stands`]): what comes out before it references pictures
        // the seek skipped past, and decodes to a grey field with the corner of
        // a logo in it. Nothing is fed to the decoder until a key packet has
        // arrived for the same reason --
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
        let mut reading = untimed.paired(ictx.read_packets(), idx);
        while let Some((s, mut packet, doubted)) = reading.read() {
            if s.index() != idx {
                continue;
            }
            untimed.place(&mut packet, doubted);
            in_force.see(&packet);
            if until.is_some_and(|until| {
                packet.dts().is_some_and(|dts| dts as f64 * time_base - start > until)
            }) {
                break;
            }
            started = started || packet.is_key();
            if !started {
                // A recording that flags no key packet at all -- and there are
                // containers that flag none -- is read from wherever the seek
                // put it rather than to the end and answered with nothing.
                waiting += 1;
                started = waiting > 300;
                unkeyed = started;
                if !started {
                    continue;
                }
            }
            left -= 1;
            if left <= 0 {
                break;
            }
            // **An entry picture libavformat does not flag**: an AVCHD
            // disc's map enters on I pictures that are neither IDR nor
            // recovery points, so its packets flag no key at all and the
            // read above starts for want of one. libavcodec decodes such a
            // picture and never shows it, and the glance came back with no
            // picture; told to show it, as `walk` is, the glance stands on
            // it. Only where no key packet started the read.
            if unkeyed
                && !packet.is_key()
                && h264.is_some_and(|framing| {
                    crate::bitstream::h264_unmarked_intra(packet.data().unwrap_or(&[]), framing)
                })
            {
                unsafe {
                    (*decoder.as_mut_ptr()).flags2 |= ff::ffi::AV_CODEC_FLAG2_SHOW_ALL;
                }
            }
            if let Some(p) = palette_owed.take() {
                give_palette(&mut packet, p);
            }
            in_force.mend(&mut packet, &mut |at, record| lend(lent, spec).map(|src| sets_before(src, at, record)));
            if vols.asks(&packet) {
                if let Some(src) = lend(lent, spec) {
                    vols.put(&mut packet, src, time_base);
                }
            } else {
                vols.note(&packet);
            }
            if decoder.send_packet(&packet).is_err() {
                continue;
            }
            while decoder.receive_frame(&mut frame).is_ok() {
                let kind = kind_of(&frame);
                if stands(&frame) {
                    return Ok((
                        Shot {
                            jpeg: encode_jpeg(&frame, sar, width)?,
                            time: when(&frame),
                            kind,
                        },
                        true,
                    ));
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
            if stands(&frame) {
                return Ok((
                    Shot {
                        jpeg: encode_jpeg(&frame, sar, width)?,
                        time: when(&frame),
                        kind,
                    },
                    true,
                ));
            }
            if spare.is_none() {
                spare = Some(Shot {
                    jpeg: encode_jpeg(&frame, sar, width)?,
                    time: when(&frame),
                    kind,
                });
            }
        }
        spare.map(|s| (s, false)).ok_or_else(|| match landing {
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
            spec,
            ictx,
            decoder,
            idx,
            time_base,
            sar,
            start,
            used,
            in_force,
            lent,
            untimed,
            vols,
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
            crate::input::head_seek(ictx)
        };
        let _ = ictx.seek(ts, ..ts);
        in_force.restart();
        untimed.restart();
        vols.restart();

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
        // One decoded picture into its cell; true once the stretch is done.
        let mut take = |frame: &ff::frame::Video, out: &mut Vec<Shot>| -> Result<bool> {
            let at = when(frame);
            // Pictures come out of the decoder in presentation order, so
            // the first one past the far end says the stretch is done.
            if at > to + 1e-6 {
                return Ok(true);
            }
            if at < from - 1e-6 {
                return Ok(false);
            }
            // One picture per cell is all the caller can show, and the
            // decoding is done either way -- what this saves is the
            // encoding of the ones that would land on top of each other.
            let k = cell_of(at);
            if k == held {
                return Ok(false);
            }
            held = k;
            out.push(Shot {
                jpeg: encode_jpeg(frame, sar, width)?,
                time: at,
                kind: kind_of(frame),
            });
            Ok(out.len() >= most)
        };
        // Whether the read stopped before the file did. Where it did not, the
        // decoder still holds the last pictures it was given for reordering,
        // and they are the end of the recording: a stretch reaching the end
        // of a recording that has B pictures had its last cells empty.
        let mut done = false;
        let mut reading = untimed.paired(ictx.read_packets(), idx);
        'read: while let Some((s, mut packet, doubted)) = reading.read() {
            if s.index() != idx {
                continue;
            }
            untimed.place(&mut packet, doubted);
            in_force.see(&packet);
            // Counted before the wait for a key packet as well as after it:
            // a container that flags none never ends that wait, and the
            // strip read to the end of the file on every update.
            left -= 1;
            if left <= 0 {
                done = true;
                break;
            }
            started = started || packet.is_key();
            if !started {
                continue;
            }
            in_force.mend(&mut packet, &mut |at, record| lend(lent, spec).map(|src| sets_before(src, at, record)));
            if vols.asks(&packet) {
                if let Some(src) = lend(lent, spec) {
                    vols.put(&mut packet, src, time_base);
                }
            } else {
                vols.note(&packet);
            }
            if decoder.send_packet(&packet).is_err() {
                continue;
            }
            while decoder.receive_frame(&mut frame).is_ok() {
                if take(&frame, &mut out)? {
                    done = true;
                    break 'read;
                }
            }
        }
        if !done && started {
            let _ = decoder.send_eof();
            while decoder.receive_frame(&mut frame).is_ok() {
                if take(&frame, &mut out)? {
                    break;
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

/// The quantiser every picture the timeline and the player show is encoded
/// at: a strip cell, a stage frame thirty times a second. Small and quick
/// enough for that, and soft once a picture is looked at on its own.
const JPEG_Q: i32 = 4;

/// The quantiser for a picture put up to be looked at for a while -- the
/// output screen's view of what is being re-encoded. A handful of them, each
/// on screen for as long as a segment takes to write, so the size a finer
/// one comes to is nothing set against seeing the frame as it is.
const JPEG_Q_FINE: i32 = 2;

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
    encode_jpeg_within(picture, sar, width.min(native), JPEG_Q)
}

/// `encode_jpeg_sized` with the width already settled by the caller: a field
/// stands for a frame twice its height, and is worth the frame's width.
fn encode_jpeg_within(
    picture: &ff::frame::Video,
    sar: f64,
    width: u32,
    q: i32,
) -> Result<(Vec<u8>, u32, u32)> {
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
    // And into the matrix a JPEG is read with, which is BT.601 whatever the
    // picture was made with. Left in the picture's own, an HD recording's
    // BT.709 samples were read through BT.601 by the webview: the editor,
    // the strip and a cover showed its reds orange and its greens yellow
    // against the cut every player shows, and against the still saved of the
    // same frame (see [`to_rgb`]). After the scaling, at the JPEG's size, and
    // here rather than by libswscale, which goes through 16-bit RGB at the
    // size it is asked for: that more than doubled what a 1080p picture cost
    // to play at 1920 across (17 to 26 ms).
    if let Some(from) = jpeg_matrix(picture) {
        rematrix(&mut scaled, from);
    }

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
        (*enc.as_mut_ptr()).global_quality = ff::ffi::FF_QP2LAMBDA * q;
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

/// The YUV matrix `picture` is read with, as its two luma weights (red,
/// blue), where it is not the BT.601 one a JPEG is read with; `None` where it
/// is, or where the picture is not YUV. By the rule [`to_rgb`] reads it by:
/// what the picture says, or by its size where it says nothing.
fn jpeg_matrix(picture: &ff::frame::Video) -> Option<(f64, f64)> {
    use ff::ffi::AVColorSpace as S;
    let yuv = unsafe {
        let d = ff::ffi::av_pix_fmt_desc_get(picture.format().into());
        !d.is_null()
            && (*d).nb_components >= 3
            && (*d).flags & (ff::ffi::AV_PIX_FMT_FLAG_RGB as u64) == 0
    };
    if !yuv {
        return None;
    }
    match unsafe { (*picture.as_ptr()).colorspace } {
        S::AVCOL_SPC_BT709 => Some((0.2126, 0.0722)),
        S::AVCOL_SPC_BT2020_NCL | S::AVCOL_SPC_BT2020_CL => Some((0.2627, 0.0593)),
        S::AVCOL_SPC_FCC => Some((0.30, 0.11)),
        S::AVCOL_SPC_SMPTE240M => Some((0.212, 0.087)),
        S::AVCOL_SPC_BT470BG | S::AVCOL_SPC_SMPTE170M => None,
        // Every other answer by the size, as [`to_rgb`] reads it: not only
        // an unstated one. A matrix neither knows (YCgCo, a reserved value)
        // on an HD picture was BT.709 in the still and BT.601 here.
        _ if picture.width() >= 1280 || picture.height() > 576 => Some((0.2126, 0.0722)),
        _ => None,
    }
}

/// Bring a full-range 4:2:0 picture's samples from the YUV matrix whose luma
/// weights are `(kr, kb)` to BT.601's, in place: through RGB, which is
/// linear, so three sums a sample. Each luma sample takes the chroma of the
/// 2x2 block it is in.
fn rematrix(frame: &mut ff::frame::Video, (kr, kb): (f64, f64)) {
    const ONE: f64 = (1 << 14) as f64;
    let (kr2, kb2) = (0.299, 0.114);
    // The new (Y, Cb, Cr) of each of the old ones alone, Cb and Cr centred.
    let to601 = |y: f64, cb: f64, cr: f64| {
        let r = y + 2.0 * (1.0 - kr) * cr;
        let b = y + 2.0 * (1.0 - kb) * cb;
        let g = (y - kr * r - kb * b) / (1.0 - kr - kb);
        let y2 = kr2 * r + (1.0 - kr2 - kb2) * g + kb2 * b;
        (y2, (b - y2) / (2.0 * (1.0 - kb2)), (r - y2) / (2.0 * (1.0 - kr2)))
    };
    let fix = |v: f64| (v * ONE).round() as i32;
    let on_cb = to601(0.0, 1.0, 0.0);
    let on_cr = to601(0.0, 0.0, 1.0);
    let (y_cb, cb_cb, cr_cb) = (fix(on_cb.0), fix(on_cb.1), fix(on_cb.2));
    let (y_cr, cb_cr, cr_cr) = (fix(on_cr.0), fix(on_cr.1), fix(on_cr.2));
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (ys, cbs, crs) = (frame.stride(0), frame.stride(1), frame.stride(2));
    let clamp = |v: i32| ((v + (1 << 13)) >> 14).clamp(0, 255) as u8;
    // Chroma of the whole picture first, kept as it was: the luma needs it.
    let mut cb_of = vec![0i32; cw * ch];
    let mut cr_of = vec![0i32; cw * ch];
    {
        let cb = frame.data(1);
        let cr = frame.data(2);
        for y in 0..ch {
            for x in 0..cw {
                cb_of[y * cw + x] = i32::from(cb[y * cbs + x]) - 128;
                cr_of[y * cw + x] = i32::from(cr[y * crs + x]) - 128;
            }
        }
    }
    {
        let luma = frame.data_mut(0);
        for y in 0..h {
            let row = &mut luma[y * ys..y * ys + w];
            let c = (y / 2) * cw;
            for (x, v) in row.iter_mut().enumerate() {
                let (cb, cr) = (cb_of[c + x / 2], cr_of[c + x / 2]);
                *v = clamp((i32::from(*v) << 14) + y_cb * cb + y_cr * cr);
            }
        }
    }
    for (plane, (on_b, on_r)) in [(1usize, (cb_cb, cb_cr)), (2, (cr_cb, cr_cr))] {
        let stride = frame.stride(plane);
        let data = frame.data_mut(plane);
        for y in 0..ch {
            for x in 0..cw {
                let k = y * cw + x;
                let v = on_b * cb_of[k] + on_r * cr_of[k];
                data[y * stride + x] = clamp(v + (128 << 14));
            }
        }
    }
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
    // The picture a still of the same frame is saved from ([`still_picture`]),
    // which is the coded picture nearest `time` wherever the recording is
    // not woven. Where it is, the nearest was the other picture of the two a
    // frame is woven from wherever the frame begins on a top field, and a
    // cover and a still taken at the same frame were two pictures.
    let picture = still_picture(src, time)?;
    // Held to one a picture can have before it is halved for a field, as a
    // still's is: clamped only after halving, a crafted 1:1000 gave a cover
    // 19 pixels wide, and a crafted 1000:1 one half as tall as the frame's.
    let sar = credible_sar(src.video.sample_aspect_ratio);
    let (jpeg, width, height) = if picture.is_interlaced() && picture.height() >= 32 {
        // Half the lines, each standing for two: twice as tall a pixel. Sized
        // as the frame it stands for (1080i gives 1920x1080, not 960x540):
        // the field has every sample across, only the lines are halved.
        let native = (picture.width() as f64 * sar).round() as u32;
        encode_jpeg_within(&top_field(&picture), sar / 2.0, POSTER_WIDTH.min(native), JPEG_Q)?
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
        let began = walk(src, from, margin, false, false, Cores::One, false, None, true, |t, frame| {
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
    // and up is BT.709, anything smaller BT.601. HD by the rule the cut uses
    // for the same question ([`crate::cut_conform::matrices`], mpv's): 1280
    // across or more than 576 lines. By line count alone a 720p film cropped
    // to 1280x536 was saved as a still through BT.601 while its cut, and
    // every player, read it as BT.709.
    let space = match picture.color_space() {
        ff::color::Space::BT709 => ffi::SWS_CS_ITU709,
        ff::color::Space::BT2020NCL | ff::color::Space::BT2020CL => ffi::SWS_CS_BT2020,
        ff::color::Space::FCC => ffi::SWS_CS_FCC,
        ff::color::Space::SMPTE240M => ffi::SWS_CS_SMPTE240M,
        ff::color::Space::BT470BG | ff::color::Space::SMPTE170M => ffi::SWS_CS_ITU601,
        _ if picture.width() >= 1280 || picture.height() > 576 => ffi::SWS_CS_ITU709,
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
///
/// Answers where a seek by byte put it (see [`Landed`]); `None` after a seek
/// by the clock.
fn place(ictx: &mut crate::input::Demux, src: &Source, from: f64, margin: f64) -> Result<Option<Landed>> {
    // **An H.264 program stream is read from the cut's margin before, never
    // from the entry point itself** (see [`Untimed::reads_back`]); what comes
    // before the entry point is read for the order alone ([`Ahead`]).
    let margin = if margin == 0.0 && Untimed::reads_back(src) { src.seek_margin } else { margin };
    // The index knows the byte `from` begins at, so on the first attempt
    // there is nothing to approximate and no margin to spend. The timestamp
    // seek below is what is left for the containers and indexes that cannot
    // say -- and for the second attempt, which only happens now when the
    // first somehow still landed late.
    if margin == 0.0 {
        if let Some(at) = crate::index::seek_to_entry(ictx, src, from) {
            return Ok(Landed::at(src, at));
        }
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
        crate::input::head_seek(ictx)
    } else {
        ((landing + src.start_time) * ff::ffi::AV_TIME_BASE as f64) as i64
    };
    let _ = ictx.seek(target, ..target);
    Ok(None)
}

/// **A picture whose own time is damage, put back where the cut copies
/// it**: a key picture where the walk put it (`walked_place` in
/// [`crate::cut`]), and any other by the rule the walk and the cut place one
/// with, asked as the cut asks it (`stray_place` there, which reads ahead as
/// far as the question takes; the reader reads those packets next, through
/// [`Strays::read`]). Decoded at its damaged time, 4444 s on, a still
/// anywhere in its GOP was the picture before it, and a film strip's entry
/// run (which stops at the first entry picture past what it was asked for)
/// stopped there and left that cell and every one after it empty; a P
/// picture hit that way was never shown at its place -- the editor's J and
/// arrow keys stood on the picture before it for the 19 pictures up to the
/// next key, and playback stopped there. Asked only of the first picture
/// read and of one shown more than [`crate::index::STRAY`] after or before
/// the one read before it ([`crate::index::far_from`]).
pub(crate) struct Strays<'a> {
    video: usize,
    /// The pictures' times as read (or put back), the last first.
    recent: std::collections::VecDeque<f64>,
    /// The last picture's decode time, where the demuxer stated one.
    last_dts: Option<f64>,
    /// The time the last picture handed out by [`Strays::read`] came with,
    /// before anything placed one: a decode time is the packet's own only
    /// with it.
    own: Option<i64>,
    /// What the question read ahead, which is read next.
    ahead: std::collections::VecDeque<(ff::format::stream::Stream<'a>, ff::Packet, bool)>,
}

impl<'a> Strays<'a> {
    pub(crate) fn new(src: &Source) -> Self {
        Strays {
            video: src.video.stream_index,
            recent: Default::default(),
            last_dts: None,
            own: None,
            ahead: Default::default(),
        }
    }

    /// The next packet: one read ahead by [`Strays::mend`], else the next
    /// of `reading`.
    pub(crate) fn read<I>(
        &mut self,
        reading: &mut crate::cut::poc_seam::Paired<'a, I>,
    ) -> Option<(ff::format::stream::Stream<'a>, ff::Packet, bool)>
    where
        I: Iterator<Item = (ff::format::stream::Stream<'a>, ff::Packet)>,
    {
        let item = self.ahead.pop_front().or_else(|| reading.read())?;
        if item.0.index() == self.video {
            self.own = item.1.pts();
        }
        Some(item)
    }

    /// The picture just read (through [`Strays::read`]) put back where its
    /// time is damage.
    pub(crate) fn mend<I>(
        &mut self,
        src: &Source,
        packet: &mut ff::Packet,
        time_base: f64,
        reading: &mut crate::cut::poc_seam::Paired<'a, I>,
    ) where
        I: Iterator<Item = (ff::format::stream::Stream<'a>, ff::Packet)>,
    {
        let Some(pts) = packet.pts() else { return };
        let start = src.start_time;
        let mut t = pts as f64 * time_base - start;
        // Its own decode time, as the copy takes it.
        let dts = packet.dts().filter(|_| self.own == Some(pts)).map(|d| d as f64 * time_base - start);
        if self.recent.front().is_none_or(|&before| crate::index::far_from(t, before)) {
            let walked = crate::cut::walked_place(src, packet, t);
            let video = self.video;
            let Strays { recent, last_dts, ahead, .. } = self;
            if let Some(at) = crate::cut::stray_place(
                t,
                dts,
                *last_dts,
                walked,
                src.video.frame_duration(),
                recent,
                ahead,
                reading,
                |p| {
                    (p.0.index() == video)
                        .then(|| p.1.pts().map(|q| q as f64 * time_base - start))
                        .flatten()
                        .map(|q| (q, p.1.dts().map_or(q, |d| d as f64 * time_base - start)))
                },
            ) {
                packet.set_pts(Some(((at + start) / time_base).round() as i64));
                t = at;
            }
        }
        self.recent.push_front(t);
        self.recent.truncate(crate::index::REORDERED);
        self.last_dts = dts;
    }
}

/// The entry point a seek by byte ([`crate::index::seek_to_entry`]) put a
/// read at: its picture's instant and the byte the index found it at.
///
/// **An MPEG program stream read from there times its entry picture with the
/// next picture's time.** `ffmpeg -f mpeg` / `-f vob` -- and a VCD's or an
/// old capture's `.mpg` -- packs the pictures into PES packets without
/// regard for where they begin, so the packet an I picture begins in opens
/// on the tail of the picture before it. Read on from an earlier picture the
/// parser knows that tail is the old picture's and gives the I its PES's
/// time. Read from that byte, it hands the tail over as a packet of its own,
/// the PES's time goes with it, and the I picture is given the time of the
/// PES after -- the next P picture's, which then comes out a second time
/// under its own. A frame asked for at that P was answered with the I, the
/// strip's cell there was the I, and played from a range head the I was
/// shown first and then the head again: one picture a GOP of an MPEG-1
/// `.mpg` or a `.vob` rip, where [`crate::cut`], which reads on from a
/// margin before, wrote the right one. A DVD's own `.VOB` begins every GOP
/// on a pack of its own, as a transport stream begins every picture on a PES
/// of its own, and is not touched: there the entry picture carries its own
/// time already.
#[derive(Clone, Copy)]
pub(crate) struct Landed {
    time: f64,
    pos: i64,
    /// The next entry point's instant: a key picture stated at or past it is
    /// another GOP's, whatever byte it came from.
    next: f64,
}

impl Landed {
    /// The entry point at `time`, as [`crate::index::seek_to_entry`]
    /// answers it, where it has a byte.
    ///
    /// `None` on a transport stream, which begins every picture on a PES of
    /// its own and so never hands the entry picture over mistimed -- and
    /// whose entry points can be the ones that are off: a Blu-ray's map
    /// keeps a time to 512 ticks of 90 kHz (5.7 ms), and at 59.94 frames a
    /// second that is more than the quarter of a frame `mend` allows. Mended
    /// to the map's time, a BS4K recorder title's entry picture -- one in
    /// four of them -- was answered 5 ms before it is shown.
    pub(crate) fn at(src: &Source, time: f64) -> Option<Landed> {
        if src.on_a_ts {
            return None;
        }
        let k = src.points.iter().rposition(|p| p.time == time && p.pos >= 0)?;
        let p = &src.points[k];
        let next = src.points[k + 1..]
            .iter()
            .map(|q| q.time)
            .find(|&t| t > p.time)
            .unwrap_or(f64::INFINITY);
        Some(Landed { time: p.time, pos: p.pos, next })
    }

    /// Give the entry picture's packet the entry point's own time, if the
    /// read handed it over with another one. For each packet of the pictures
    /// in turn, from the landing; it stops looking at the first picture that
    /// opens a GOP, or once the read is past the entry point's byte without
    /// one -- a packet the demuxer read from the file itself, there, is
    /// timed by the file and left as it is.
    pub(crate) fn mend(slot: &mut Option<Landed>, packet: &mut ff::Packet, src: &Source) {
        let Some(at) = *slot else { return };
        let pos = packet.position() as i64;
        if !packet.is_key() {
            if pos > at.pos {
                *slot = None;
                return;
            }
            // A field picture's second field in front of the entry picture.
            // The PES the entry point begins in can open on the second field
            // of the pair before the I -- an MPEG-2 broadcast remuxed into a
            // program stream codes its pictures as fields -- and read from
            // that byte the parser takes the lone field for the first of a
            // pair and hands it over joined to the I picture, as a packet
            // that is not marked key and is timed as the field was. Fed to
            // the decoder, the I was lost with it and every picture of the
            // GOP came out a grey field: a frame asked for there, the still
            // saved there and a range played from there (one GOP of a
            // minute's remux of a BS recording). The field belongs to the
            // GOP before, which nothing read from here shows; what is left
            // from the entry picture's own headers on is its packet.
            let mpeg = matches!(src.video.codec.as_str(), "mpeg1video" | "mpeg2video");
            match packet.data().filter(|_| mpeg).and_then(entry_start) {
                Some(start) => {
                    let Some(data) = packet.data() else { return };
                    let mut own = ff::Packet::copy(&data[start..]);
                    own.set_stream(packet.stream());
                    own.set_pts(packet.pts());
                    own.set_dts(packet.dts());
                    own.set_position(packet.position());
                    own.set_flags(packet.flags() | ff::packet::Flags::KEY);
                    *packet = own;
                }
                None => return,
            }
        }
        *slot = None;
        let tb = src.video.time_base;
        let quarter = src.video.frame_duration() / 4.0;
        let stated = packet.pts().map(|p| p as f64 * tb - src.start_time);
        if stated.is_some_and(|t| (t - at.time).abs() <= quarter) || tb.is_nan() || tb <= 0.0 {
            return;
        }
        // The first key picture read, with nothing past the entry point's
        // byte before it (a packet that was is the non-key case above), is
        // the entry picture, from whichever byte the parser says it began
        // at: told by its byte alone, an I picture whose PES opened on the
        // tail of the picture before was handed over from the next pack
        // (2048 bytes on) -- `ffmpeg -f vob` of a BS broadcast, one GOP in
        // sixty -- still timed as the P picture after it, and a frame asked
        // for at that P was answered with the I. A key picture stated at
        // the next entry point or past it is that one's.
        if pos >= 0 && pos != at.pos && !stated.is_some_and(|t| t > at.time && t < at.next - quarter) {
            return;
        }
        packet.set_pts(Some(((at.time + src.start_time) / tb).round() as i64));
    }
}

/// Where an MPEG-1/2 entry picture's own headers begin inside a packet that
/// holds something else in front of them (see [`Landed::mend`]): the sequence
/// header, else the GOP header, before the first I picture, else that
/// picture's own header. `None` where the packet holds no I picture, or
/// nothing stands in front of it.
fn entry_start(data: &[u8]) -> Option<usize> {
    // Start codes: 00 00 01 xx. A picture header's coding type is three bits
    // after its ten-bit temporal reference; 1 is an I picture.
    let codes = || {
        data.windows(4)
            .enumerate()
            .filter(|(_, w)| w[0] == 0 && w[1] == 0 && w[2] == 1)
            .map(|(i, w)| (i, w[3]))
    };
    let picture = codes()
        .find(|&(i, c)| c == 0x00 && data.get(i + 5).is_some_and(|b| (b >> 3) & 7 == 1))?
        .0;
    let start = codes()
        .take_while(|&(i, _)| i < picture)
        .find(|&(_, c)| c == 0xB3)
        .or_else(|| codes().take_while(|&(i, _)| i < picture).find(|&(_, c)| c == 0xB8))
        .map_or(picture, |(i, _)| i);
    (start > 0).then_some(start)
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
    sets: bool,
    mut visit: impl FnMut(f64, &ff::frame::Video) -> bool,
) -> Result<Option<f64>> {
    let mut ictx = crate::input::demux(&src.input.url)?;
    let idx = src.video.stream_index;
    let in_tb = src.video.time_base;
    let mut landed = place(&mut ictx, src, from, margin)?;
    // Nothing but the pictures is read here, and on some recordings a stream
    // left switched on costs the pictures themselves. See
    // [`crate::input::keep_only`], and note that it goes after the seek.
    crate::input::keep_only(&mut ictx, &[idx]);

    let params = ictx
        .stream(idx)
        .ok_or_else(|| anyhow!("stream {idx} vanished"))?
        .parameters();
    // The sets in force where the read opens, for a key picture that carries
    // none (see [`InForce`]); `sets` is false for [`pictures_in`] on a
    // transport stream, whose caller ([`crate::verify`]) opens such a read on
    // the IDR that restated them instead.
    let mut in_force = if sets { InForce::single(&params, &src.video) } else { InForce::off() };
    // And the MPEG-4 Part 2 VOL in force: see [`VolInForce`].
    let mut vols = if sets { VolInForce::single(&params) } else { VolInForce::off() };
    // Before the record goes to the decoder; used below.
    let mut untimed = Untimed::knowing(&src.video, &params);
    // And an MPEG-4 Part 2 picture with no time: see
    // [`crate::cut::Mpeg4Untimed`].
    let mut vol_untimed = crate::cut::Mpeg4Untimed::default();
    let record = params.clone();
    // See [`crate::cut::Mpeg4LeadingB`].
    let mut leading_b = crate::cut::Mpeg4LeadingB::new(&params);
    // The palette of a palette recording, owed to the first picture sent:
    // see [`head_palette`].
    let mut palette_owed = head_palette(src, &params);
    // What `place` read ahead of the entry point, for the order alone: see
    // [`Ahead`]. Not where the caller asked for a margin of its own.
    let mut ahead = if margin == 0.0 { Ahead::new(src, from) } else { None };
    let mut decoder = match cores {
        Cores::One => crate::video_context(params)?.decoder().video()?,
        Cores::All => crate::video_decoder(params)?,
        Cores::Steady => steady_decoder(params)?,
    };

    let mut frame = ff::frame::Video::empty();
    let mut first = None;
    // Whether a key picture has gone into the decoder: see where an entry
    // point libavformat does not flag is fed.
    let mut keyed_in = false;
    let mut stopped = false;
    let mut entries = crate::EntryPictures::of(src);
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
    // A picture whose packet carries no time is given the one its order
    // says, as the cut gives it: see [`Untimed`] (made above, from the record).
    let mut packets = untimed.paired(ictx.read_packets(), idx);
    // See [`Strays`].
    let mut strays = Strays::new(src);
    'outer: while let Some((stream, mut packet, doubted)) = strays.read(&mut packets) {
        if stream.index() != idx {
            continue;
        }
        Landed::mend(&mut landed, &mut packet, src);
        untimed.place(&mut packet, doubted);
        if let Some(pts) = vol_untimed.pts_of(src, &packet, in_tb, || crate::cut::mpeg4_config(&record)) {
            packet.set_pts(Some(pts));
        }
        // A key picture whose own time is damage: see [`Strays`].
        strays.mend(src, &mut packet, in_tb, &mut packets);
        in_force.see(&packet);
        if Ahead::before(&mut ahead, &packet, src) {
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
        if leading_b.skip(&packet) {
            continue;
        }
        in_force.mend(&mut packet, &mut |at, record| Some(sets_before(src, at, record)));
        vols.mend(&mut packet, src, in_tb);
        // An entry point of an AVCHD disc's map that is an I picture with no
        // IDR and no recovery point, which libavcodec would never show from:
        // shown from, as the cut's re-encode is (see `reencode_segment` in
        // [`crate::cut`]), so that `--verify`, a still and the editor see
        // the pictures the cut writes. Only before any key picture has gone
        // in and anything has come out.
        if !keyed_in
            && first.is_none()
            && !packet.is_key()
            && src.video.codec == "h264"
            && crate::bitstream::h264_unmarked_intra(packet.data().unwrap_or(&[]), src.video.framing)
        {
            unsafe {
                (*decoder.as_mut_ptr()).flags2 |= ff::ffi::AV_CODEC_FLAG2_SHOW_ALL;
            }
        }
        keyed_in |= packet.is_key();
        if let Some(palette) = palette_owed.take() {
            give_palette(&mut packet, &palette);
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
    let unread = packets.inner().finished().err();
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

/// **The packets a read that [`Untimed::reads_back`] takes in before the
/// entry point it wants**: read, so that [`Untimed`] has measured the step its
/// pictures with no time are placed by, and nothing more -- not decoded, so
/// that the read decodes what it decoded from the entry point itself (a still
/// of a 1080p `.mpg` read three GOPs back and decoded from there took five
/// times as long). The first key picture at the entry point (to a quarter of
/// a frame) or past it is where decoding begins; and an earlier key picture
/// that could not be timed even so, so that a read whose order never got a
/// step decodes from there rather than not at all.
pub(crate) struct Ahead {
    entry: f64,
}

impl Ahead {
    /// For a read of `src` sought `seek_margin` before the entry point at
    /// `entry`; `None` where the read is not one of those.
    pub(crate) fn new(src: &Source, entry: f64) -> Option<Self> {
        Untimed::reads_back(src).then_some(Ahead { entry })
    }

    /// Whether `packet`, of the pictures, placed, comes before the entry
    /// point's key picture, and so is not to be decoded. Clears `slot` at
    /// that key picture.
    pub(crate) fn before(slot: &mut Option<Ahead>, packet: &ff::Packet, src: &Source) -> bool {
        let Some(ahead) = slot.as_ref() else { return false };
        if packet.is_key() {
            let at = packet.pts().map(|p| p as f64 * src.video.time_base - src.start_time);
            if at.is_none_or(|t| t >= ahead.entry - src.video.frame_duration() / 4.0) {
                *slot = None;
                return false;
            }
        }
        true
    }
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
#[derive(Clone)]
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
    /// says nowhere and leaves the read where the last one put it. True
    /// where that took the read across a seam -- never on a recording
    /// without one.
    pub(crate) fn reach(&mut self, pos: isize) -> bool {
        if pos < 0 {
            return false;
        }
        let was = self.next;
        while let Some(j) = self.joins.get(self.next).filter(|j| pos as u64 >= j.at) {
            self.since = Some(j.time);
            self.next += 1;
        }
        self.next != was
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
///
/// `sets`: the entry picture is given the H.264 sets or the MPEG-4 Part 2
/// VOL in force where it carries none ([`InForce`], [`VolInForce`]), as the
/// cut gives its copy.
pub(crate) fn pictures_in(
    src: &Source,
    from: f64,
    to: f64,
    head: Option<f64>,
    sets: bool,
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
        let Some(t) = next else {
            // Not where that picture had gone before `from` and nothing came
            // after it: the range lies past the last picture, and the cut
            // writes none there (`past_the_pictures` in its re-encode). Gone
            // by its own stated length, or by its coded one on a recording
            // that comes at a rate.
            let stated = unsafe { (*held.as_ptr()).duration };
            let ended = if stated > 0 {
                Some(was + stated as f64 * src.video.time_base)
            } else if !src.video.variable_rate {
                let shown_for = 2 + unsafe { (*held.as_ptr()).repeat_pict.max(0) };
                Some(was + f64::from(shown_for) * fd / 2.0)
            } else {
                None
            };
            // Only where the read ran to the end of the file: one stopped at a
            // seam (`wall`, below) has pictures after it, and the cut, drained
            // there, holds the picture as it always did.
            let to_the_end = !src.joins.iter().any(|j| j.time >= to - eps);
            // And only over a frame past it, as the cut: see
            // `past_the_pictures` there.
            return head.is_some()
                && !(to_the_end
                    && ended.is_some_and(|end| end <= from + eps && to <= end + fd + eps));
        };
        if head.is_some_and(|end| t >= end - eps) {
            // **But not, on a constant-rate recording of stills, where the
            // head is under half a field** -- [`crate::cut::still_up_from`]
            // has no place for the picture, and the cut writes none there
            // (see the end of `reencode_segment` in [`crate::cut`]).
            let stills_cfr = crate::cut::comes_as_stills(&src.video) && !src.video.variable_rate;
            return !(stills_cfr
                && head.is_some_and(|end| t <= end + eps)
                && crate::cut::still_up_from(src, from, t).is_none());
        }
        // On the closest two pictures can come, not the average, as the
        // re-encode in `cut` asks it (see `VideoInfo::least_gap`): a frame on
        // every recording whose pictures come at a rate.
        let least = src.video.least_gap();
        let shown_for = 2 + unsafe { (*held.as_ptr()).repeat_pict.max(0) };
        let own_end = was + f64::from(shown_for) * least / 2.0;
        // And always on a recording of stills, as the cut keeps it there
        // ([`crate::cut::still_up_from`]).
        (t - from >= least && own_end < t - least / 4.0) || crate::cut::still_up_from(src, from, t).is_some()
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
        walk(src, entry, margin, false, false, Cores::Steady, true, wall, sets, |t, frame| {
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

/// **A palette recording's palette, which only its first packet carries**,
/// for a read that does not start there.
///
/// QuickTime's and AVI's palette pictures (`raw ` or `qtrle` at 8 bits or
/// fewer, an 8-bit `.avi`) keep their palette in the stream's header, and
/// libavformat hands it on once, as `AV_PKT_DATA_PALETTE` on the first
/// packet read after the open -- which the open's own probe reads, and the
/// seek here throws away. The decoder, given no palette, painted every
/// picture black: a still, the editor's picture and `--verify` (which read
/// the cut the same way and so called a cut with no palette right). The cut
/// gives it back to its copy the same way (`head_palette` in [`crate::cut`]).
///
/// Read off an opening of its own, and only where there can be one: a stream
/// of eight bits a pixel or fewer (33 to 40 are QuickTime's greys), or one
/// libav already calls a palette. Every other recording opens nothing more.
fn head_palette(src: &Source, params: &ff::codec::Parameters) -> Option<Vec<u8>> {
    palette_of(&src.input.url, src.video.stream_index, params)
}

/// [`head_palette`] of the stream `idx` of `url`.
fn palette_of(url: &str, idx: usize, params: &ff::codec::Parameters) -> Option<Vec<u8>> {
    // SAFETY: the parameters are the stream's own, read in place.
    let (bits, format) = unsafe { ((*params.as_ptr()).bits_per_coded_sample, (*params.as_ptr()).format) };
    if !(matches!(bits, 1..=8 | 33..=40) || format == ff::ffi::AVPixelFormat::AV_PIX_FMT_PAL8 as i32) {
        return None;
    }
    let mut head = crate::input::demux(url).ok()?;
    // The first picture, which is at most a few packets in.
    for (stream, packet) in head.read_packets().take(64) {
        if stream.index() == idx {
            return packet_palette(&packet);
        }
    }
    None
}

/// A packet's `AV_PKT_DATA_PALETTE`, where it has one.
pub(crate) fn packet_palette(packet: &ff::Packet) -> Option<Vec<u8>> {
    let mut size = 0;
    // SAFETY: a lookup in the packet's own side data; the bytes are copied
    // out before the packet can go.
    unsafe {
        let p = ff::ffi::av_packet_get_side_data(
            ff::packet::Ref::as_ptr(packet),
            ff::ffi::AVPacketSideDataType::AV_PKT_DATA_PALETTE,
            &mut size,
        );
        (!p.is_null() && size > 0).then(|| std::slice::from_raw_parts(p, size).to_vec())
    }
}

/// Puts the recording's palette on a picture that came without one; one
/// that has its own keeps it. See [`head_palette`].
pub(crate) fn give_palette(packet: &mut ff::Packet, palette: &[u8]) {
    if packet_palette(packet).is_some() {
        return;
    }
    // SAFETY: new side data of the size asked for, filled at once.
    unsafe {
        let p = ff::ffi::av_packet_new_side_data(
            ff::packet::Mut::as_mut_ptr(packet),
            ff::ffi::AVPacketSideDataType::AV_PKT_DATA_PALETTE,
            palette.len(),
        );
        if !p.is_null() {
            std::ptr::copy_nonoverlapping(palette.as_ptr(), p, palette.len());
        }
    }
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
///
/// **Theora too, which conceals nothing and still differs.** libavcodec's
/// frame-threaded Theora (`vp3`) decoder gives other pictures from run to run
/// on undamaged input -- the same `.ogv` decoded six times gave five
/// different hashes, up to 17 of 661 pictures off by a few levels -- and a
/// right cut, its copied packets the source's to the byte, was called
/// "5 of 230 copied frames differ". On one thread it is the same every time.
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
            | ff::codec::Id::THEORA
            | ff::codec::Id::VP3
    );
    let mut ctx = crate::video_context(params)?;
    unsafe {
        let c = ctx.as_mut_ptr();
        (*c).thread_count = if conceals { 1 } else { 0 };
    }
    Ok(ctx.decoder().video()?)
}

/// **Where an H.264 picture whose packet carries no time is shown**, as the
/// cut places it: its order field read by the cut's own rule
/// (`poc_seam::Order` in [`crate::cut`]), so that the two cannot disagree.
///
/// A program stream puts a time on a PES, not on each picture in it, and an
/// H.264 `.mpg` (`ffmpeg -f mpeg`, a recorder's export) holds pictures that
/// come with neither pts nor dts -- about one in eleven with B pictures, more
/// without them. The cut places them ([`crate::cut`]); every reader here
/// passed them over -- the walk left them out of the index, which then said
/// "the pictures vary" of a constant 25p recording, the editor could not
/// stand on them and played the picture before each twice as long, and
/// `--verify` compared every picture after one with the wrong one and failed
/// a correct cut ("75 written, 73 expected").
///
/// Anything the rule cannot place -- a field, a header it cannot read -- is
/// passed over as before; a transport stream's second field, the usual
/// packet with no time, is a field. Off for anything but H.264 in start
/// codes.
///
/// **Read through [`Self::paired`] (or [`Self::hold`])**, which says of each
/// picture whether libav gave it the next one's time: see
/// [`crate::cut::poc_seam::Pairs`].
pub(crate) struct Untimed(
    Option<crate::cut::poc_seam::Order>,
    crate::cut::poc_seam::Pairs<ff::Packet>,
);

impl Untimed {
    /// For the video stream `video` describes, knowing no sets but those the
    /// packets restate: for a read from the head. A read that seeks takes
    /// [`Self::knowing`].
    pub(crate) fn new(video: &crate::VideoInfo) -> Self {
        Self::with(Self::wanted(video), None, Self::pace_of(video))
    }

    /// For the video stream `video` describes, whose record is `params`.
    /// **Knowing the record's sets, and the rate, as the cut's copy does**
    /// ([`crate::cut::poc_seam::Order::knowing`] and `paced`): a read that
    /// starts from a seek meets only the sets restated after it, and an x264
    /// `.mp4` remuxed to `.mpg` states them at its head alone -- no picture
    /// with no time could be placed after a seek there, so a still or the
    /// editor's picture landed on the one before or after it.
    pub(crate) fn knowing(video: &crate::VideoInfo, params: &ff::codec::Parameters) -> Self {
        Self::with(Self::wanted(video), Some(params), Self::pace_of(video))
    }

    /// Whether a read of `src` that seeks has to start [`Source::seek_margin`]
    /// before the entry point it wants, as the cut's copy starts
    /// (`seek_to` in [`crate::cut`]): H.264 in a program stream. **Its
    /// pictures with no time are placed by the step two timed ones have
    /// measured** ([`crate::cut::poc_seam::Order`]; until then by the rate,
    /// which is a guess), and a read that starts on an entry point has
    /// measured none: a second of black or a still packs fifty pictures and
    /// more into PES packets with no time, and after a key picture there the
    /// whole run was passed over -- a still asked anywhere in it was the
    /// picture before or after the run, and played from it the run was not
    /// shown, where the cut, reading from its margin before, wrote every
    /// picture of it. Read from the same margin, the order here is the one
    /// the cut measured, not a guess the cut never makes. A transport stream
    /// times every picture (a second field is passed over as before) and is
    /// read as it was.
    pub(crate) fn reads_back(src: &Source) -> bool {
        Self::wanted(&src.video) && !src.on_a_ts
    }

    fn wanted(video: &crate::VideoInfo) -> bool {
        video.codec == "h264" && video.framing == crate::bitstream::NalFraming::AnnexB
    }

    /// Ticks per picture by the stream's rate.
    fn pace_of(video: &crate::VideoInfo) -> f64 {
        video.frame_duration() / video.time_base
    }

    /// On where `on` says the pictures are H.264 in start codes, knowing the
    /// sets of `record` where there is one, and `pace` ticks per picture.
    fn with(on: bool, record: Option<&ff::codec::Parameters>, pace: f64) -> Self {
        Untimed(
            on.then(|| {
                let sets = record.map(crate::cut::record_sets_of).unwrap_or_default();
                crate::cut::poc_seam::Order::knowing(&sets).paced(pace)
            }),
            crate::cut::poc_seam::Pairs::new(None),
        )
    }

    /// After a seek: the picture before is no longer the one before.
    pub(crate) fn restart(&mut self) {
        if let Some(order) = self.0.as_mut() {
            order.restart();
        }
        self.1.restart();
    }

    /// The read `inner`, whose pictures are stream `video`, telling of each
    /// packet whether its time is the next picture's, for [`Self::place`] and
    /// [`Self::pts_of`]. Straight through where this is off.
    pub(crate) fn paired<'a, I>(&self, inner: I, video: usize) -> crate::cut::poc_seam::Paired<'a, I>
    where
        I: Iterator<Item = (ff::format::stream::Stream<'a>, ff::Packet)>,
    {
        crate::cut::poc_seam::Paired::new(inner, video, self.0.is_some())
    }

    /// For a reader that reads one picture's packet at a time: `packet` held,
    /// and the one before it handed back (with whether its time is
    /// `packet`'s), where there is one. See [`Self::release`].
    pub(crate) fn hold(&mut self, packet: ff::Packet) -> Option<(ff::Packet, bool)> {
        if self.0.is_none() {
            return Some((packet, false));
        }
        self.1.push(packet);
        self.1.pop()
    }

    /// The read is over: the packet [`Self::hold`] holds, if any.
    pub(crate) fn release(&mut self) -> Option<(ff::Packet, bool)> {
        self.1.end();
        self.1.pop()
    }

    /// The time of a packet, in the stream's ticks: its own, or the one its
    /// order says where it has none and that can be told. Every packet read
    /// goes through this in file order, timed or not. `doubted`: its time
    /// is the next picture's, as [`Self::paired`] says.
    pub(crate) fn pts_of(&mut self, packet: &ff::Packet, doubted: bool) -> Option<i64> {
        match self.0.as_mut() {
            Some(order) => order.pts_of(packet, doubted),
            None => packet.pts(),
        }
    }

    /// As [`Self::pts_of`], giving a packet the time its order says where it
    /// has none, or has the next picture's, for a decoder to hand back with
    /// the picture.
    pub(crate) fn place(&mut self, packet: &mut ff::Packet, doubted: bool) {
        if self.0.is_none() {
            return;
        }
        let pts = self.pts_of(packet, doubted);
        if pts.is_some() && pts != packet.pts() {
            packet.set_pts(pts);
        }
    }
}

/// **A decoder opened inside an MPEG-4 Part 2 recording is given the VOL in
/// force there, as the cut gives its copy.** Two Xvid encodes joined into
/// one `.avi` or `.mkv`, each stating its VOL in its first picture only,
/// keep the first one's as their record, and a decoder opened inside the
/// second was built from it: a still, a glimpse, a strip cell and the scene
/// list's pictures there decoded as the first encode's (a progressive
/// encode joined to an interlaced one: every picture of the second wrong).
/// The headers in force come from [`crate::cut::mpeg4_vol_in_force`], read
/// once per recording; a key picture that states its own VOL -- libavcodec's
/// and libxvid's habit -- asks nothing, and a recording of one encode gets
/// nothing put in.
pub(crate) struct VolInForce {
    /// The record's headers, where they state a VOL; `None`: off.
    record: Option<Vec<u8>>,
    /// Every key picture may go to another decoder (a pool), rather than
    /// only the first one after the decoder was made or wound back.
    pool: bool,
    /// The one decoder has had its first key picture since it was made or
    /// wound back.
    fed: bool,
    /// A VOL other than the record's has been met in band or put in: a
    /// decoder may hold it, so where the record is in force it is put in.
    differs: bool,
}

impl VolInForce {
    /// For one decoder, fed in file order from where it was made or wound
    /// back ([`Self::restart`]).
    pub(crate) fn single(params: &ff::codec::Parameters) -> Self {
        Self::of(params, false)
    }

    /// For the decoders of a pool, any of which may be handed any key.
    pub(crate) fn pool(params: &ff::codec::Parameters) -> Self {
        Self::of(params, true)
    }

    fn of(params: &ff::codec::Parameters, pool: bool) -> Self {
        let record = (params.id() == ff::codec::Id::MPEG4)
            .then(|| unsafe {
                let p = params.as_ptr();
                if (*p).extradata.is_null() || (*p).extradata_size <= 0 {
                    None
                } else {
                    Some(std::slice::from_raw_parts((*p).extradata, (*p).extradata_size as usize).to_vec())
                }
            })
            .flatten()
            .filter(|r| crate::cut::mpeg4_has_vol(r));
        VolInForce {
            record,
            pool,
            fed: false,
            differs: false,
        }
    }

    /// Reading and changing nothing.
    pub(crate) fn off() -> Self {
        VolInForce {
            record: None,
            pool: false,
            fed: false,
            differs: false,
        }
    }

    /// After a seek: the next key picture is the first again. What the
    /// decoder holds stays (a flush keeps the VOL).
    pub(crate) fn restart(&mut self) {
        self.fed = false;
    }

    /// Whether [`Self::mend`] would look `packet`'s time up, which is where
    /// it needs the recording: a key picture stating no VOL, its decoder's
    /// first or one of a pool's. Every other packet is only noted.
    fn asks(&self, packet: &ff::Packet) -> bool {
        self.record.is_some()
            && packet.is_key()
            && (self.pool || !self.fed)
            && !crate::cut::mpeg4_has_vol(packet.data().unwrap_or(&[]))
    }

    /// A packet about to be decoded, `tb` its stream's time base: a key
    /// picture that states no VOL, the first of its decoder's, gets the
    /// headers in force at its time in front of it.
    pub(crate) fn mend(&mut self, packet: &mut ff::Packet, src: &Source, tb: f64) {
        if self.asks(packet) {
            self.put(packet, src, tb);
        } else {
            self.note(packet);
        }
    }

    /// A packet [`Self::asks`] nothing for: one stating a VOL is what its
    /// decoder now holds.
    fn note(&mut self, packet: &ff::Packet) {
        let Some(record) = self.record.as_deref() else { return };
        let data = packet.data().unwrap_or(&[]);
        if crate::cut::mpeg4_has_vol(data) {
            // Told apart by its bytes only: a VOL stated again as the record
            // states it is the record's.
            if !data.starts_with(record) {
                self.differs = true;
            }
            self.fed = true;
        }
    }

    /// A key picture [`Self::asks`] for: the headers in force at its time,
    /// or the record's where another may be held, put in front of it.
    fn put(&mut self, packet: &mut ff::Packet, src: &Source, tb: f64) {
        let Some(record) = self.record.as_deref() else { return };
        let data = packet.data().unwrap_or(&[]);
        self.fed = true;
        let Some(t) = packet.pts().or(packet.dts()).map(|p| p as f64 * tb - src.start_time) else {
            return;
        };
        let header = match crate::cut::mpeg4_vol_in_force(src, t, record) {
            Some(h) => {
                self.differs = true;
                h
            }
            None if self.differs => record.to_vec(),
            None => return,
        };
        let mut out = ff::Packet::copy(&[header.as_slice(), data].concat());
        out.set_stream(packet.stream());
        out.set_pts(packet.pts());
        out.set_dts(packet.dts());
        out.set_duration(packet.duration());
        out.set_position(packet.position());
        out.set_flags(packet.flags());
        *packet = out;
    }
}

/// H.264 parameter sets as one recording's packets carry them.
type Sets = Vec<Vec<u8>>;

/// What [`InForce::mend`] asks what is in force in front of a byte with.
pub(crate) type AskSets<'a> = dyn FnMut(u64, &[Vec<u8>]) -> Option<(FileKey, Sets)> + 'a;

/// **A decoder opened inside a recording is given the H.264 parameter sets
/// in force there, as the cut gives its copy.** Two encodes put one after
/// the other into a transport stream (`ffmpeg -f concat -c copy`) restate
/// the second one's sets, under the same numbers, only in front of its IDR;
/// its later key pictures carry none. A decoder opened on one of those is
/// built from the stream's record -- the first encode's sets -- and decoded
/// something else: a still saved 5 s into the second encode was a 3.5 dB
/// smear, the editor stood on the same, and the film strip and the scene
/// list, whose entry pictures go to several decoders, came out different
/// from run to run with whichever of them had met the IDR. The cut was
/// right ([`crate::cut::sets_in_force`]); these now ask it the same thing.
///
/// Only an Annex-B H.264 stream, and only a key picture that restates no
/// sets reads anything: a broadcast or a disc, whose key pictures carry
/// their own, reads nothing more and decodes as it always did. A recording
/// whose later key pictures carry none but whose sets never change (a
/// remux that states them at its head) is read back once and given nothing
/// (the decoder already holds those). See [`sets_before`] for the cost.
///
/// **An MP4 or Matroska file's too, learnt as the cut learns them.** The
/// same splice remuxed into an `.mp4` (`ffmpeg -i spliced.ts -c copy`)
/// keeps the second encode's sets in band, in lengths, in front of its IDR,
/// and its record (`avcC`) holds the first encode's: the scene list of such a
/// file came out different on every run, with whichever decoder of the pool
/// had met that IDR. What the packets read restate, in file order, is put
/// in front of the key pictures after them, as `copy_segment`'s
/// `length_sets` does in the cut, and what is in force where a read opens is
/// read back key picture by key picture as the cut reads it for a copy
/// ([`sets_before`], [`crate::cut::length_sets_at`]). A file whose sets in
/// band are its record's -- every phone -- has nothing put in, and one that
/// states none in band reads one key picture more, once.
pub(crate) struct InForce {
    on: bool,
    /// How the packets frame their NAL units: start codes, or lengths of
    /// this many bytes (an `avcC` record).
    framing: crate::bitstream::NalFraming,
    /// Whether what is in force where the read opens is known and not just
    /// the record taken for want of anything better: the read opens at the
    /// head ([`Self::opens_at_head`]), or it was read back or found by an earlier
    /// read ([`known_at`]). Only then is what a length-framed read finds
    /// remembered for the next, or put in front of a key picture before the
    /// read has met a restatement of its own.
    sure: bool,
    /// The head is where the read opens.
    head: bool,
    /// Whether the packets read since `first` restated a set not in the
    /// record.
    met: bool,
    /// The key pictures a read from the head passed before anything in it
    /// was not the record's, where the record was in force: noted once
    /// something is, so that a later read opening on one knows.
    before: Vec<u64>,
    /// Whether what is in force may be asked of a length-framed file
    /// whatever has been met: false for a [`Glancer`], whose asking opens the
    /// recording again, until a read anywhere has met a set in band that is
    /// not a record's.
    ask: bool,
    record: Sets,
    /// The byte of the first packet seen since the read began (or the last
    /// [`Self::restart`]).
    first: Option<u64>,
    /// What the packets seen since `first` restated, each in place of the
    /// set of its kind and number.
    learnt: Sets,
    /// What is in force in front of `first`, once it has been asked.
    base: Option<Sets>,
    /// What the one decoder this feeds holds; `None` for the decoders of a
    /// pool, any of which can be handed any picture.
    given: Option<Sets>,
    /// Whether anything restated or in force here is not the record's.
    diverged: bool,
    /// The file `base` was answered for.
    file: Option<FileKey>,
}

impl InForce {
    /// For one decoder, freshly built from `params`, fed in file order.
    pub(crate) fn single(params: &ff::codec::Parameters, video: &crate::VideoInfo) -> Self {
        let mut s = Self::pool(params, video);
        s.given = Some(s.record.clone());
        s
    }

    /// For the decoders of a [`crate::entrypool::Pool`], each built from
    /// `params`: every set-less key picture that needs them carries them.
    pub(crate) fn pool(params: &ff::codec::Parameters, video: &crate::VideoInfo) -> Self {
        Self::of(params, video.codec == "h264")
    }

    /// Reading and changing nothing.
    pub(crate) fn off() -> Self {
        InForce {
            on: false,
            framing: crate::bitstream::NalFraming::AnnexB,
            sure: false,
            head: false,
            met: false,
            before: Vec::new(),
            ask: true,
            record: Vec::new(),
            first: None,
            learnt: Vec::new(),
            base: None,
            given: None,
            diverged: false,
            file: None,
        }
    }

    fn of(params: &ff::codec::Parameters, on: bool) -> Self {
        if !on {
            return Self::off();
        }
        let extradata = unsafe {
            let p = params.as_ptr();
            if (*p).extradata.is_null() || (*p).extradata_size <= 0 {
                &[][..]
            } else {
                std::slice::from_raw_parts((*p).extradata, (*p).extradata_size as usize)
            }
        };
        let framing = crate::bitstream::framing_from_extradata("h264", extradata);
        let record = match framing {
            crate::bitstream::NalFraming::AnnexB => crate::cut::record_sets_of(params),
            crate::bitstream::NalFraming::Length(_) => crate::bitstream::parameter_sets("h264", extradata),
        };
        InForce {
            on,
            framing,
            sure: false,
            head: false,
            met: false,
            before: Vec::new(),
            ask: true,
            record,
            first: None,
            learnt: Vec::new(),
            base: None,
            given: None,
            diverged: false,
            file: None,
        }
    }

    /// After a seek: what is in force is asked again where the read now is.
    /// What the decoder holds stays (a flush keeps the sets).
    pub(crate) fn restart(&mut self) {
        self.first = None;
        self.learnt.clear();
        self.base = None;
        self.sure = false;
        self.head = false;
        self.met = false;
        self.before.clear();
    }

    /// The read opens at the head of the recording, where the record's sets
    /// are the ones in force.
    pub(crate) fn opens_at_head(&mut self) {
        self.head = true;
    }

    /// Every packet of the pictures read, in file order, whether it is
    /// decoded or not.
    pub(crate) fn see(&mut self, packet: &ff::Packet) {
        if !self.on {
            return;
        }
        if self.first.is_none() {
            self.first = u64::try_from(packet.position()).ok();
        }
        for nal in restated(packet.data().unwrap_or(&[]), self.framing) {
            if !self.record.iter().any(|r| r.as_slice() == nal) {
                self.diverged = true;
                self.met = true;
                if matches!(self.framing, crate::bitstream::NalFraming::Length(_)) {
                    LENGTH_DIVERGED.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            put_set(&mut self.learnt, nal);
        }
    }

    /// A packet about to be decoded: a key picture that restates no sets
    /// gets the sets in force in front of it, where the decoder (or, for a
    /// pool, any of them) may hold others. `base` answers what is in force
    /// in front of a byte, from the record's sets: [`sets_before`], or
    /// `None` where it cannot be asked (the record's, then).
    pub(crate) fn mend(&mut self, packet: &mut ff::Packet, base: &mut AskSets<'_>) {
        if !self.on {
            return;
        }
        let sets = restated(packet.data().unwrap_or(&[]), self.framing);
        if !sets.is_empty() {
            if let Some(given) = self.given.as_mut() {
                for nal in sets {
                    put_set(given, nal);
                }
            }
            return;
        }
        if !packet.is_key() {
            return;
        }
        let length = matches!(self.framing, crate::bitstream::NalFraming::Length(_));
        // A read of an MP4 or Matroska file from its head that has met nothing
        // but the record's sets: the record is in force and the decoders hold
        // it. Nor does a glance ask, until something anywhere has restated
        // other sets (a glance of an ordinary `.mp4` opens nothing more).
        if length
            && !self.diverged
            && (self.head || (!self.ask && !LENGTH_DIVERGED.load(std::sync::atomic::Ordering::Relaxed)))
        {
            if self.head && self.before.len() < 1 << 14 {
                self.before.extend(u64::try_from(packet.position()).ok());
            }
            return;
        }
        let at = u64::try_from(packet.position()).ok();
        if self.base.is_none() {
            let first = self.first.or(at).unwrap_or(0);
            let (file, sets) = match base(first, &self.record) {
                Some((file, sets)) => (Some(file), sets),
                None => (None, self.record.clone()),
            };
            // See `sure`: the head, or a byte an earlier read answered for.
            self.sure = self.head || file.as_ref().is_some_and(|f| known_at(f, first));
            self.file = file;
            self.base = Some(sets);
        }
        let mut now = self.base.clone().unwrap_or_default();
        for s in &self.learnt {
            put_set(&mut now, s);
        }
        if !same(&now, &self.record) {
            self.diverged = true;
        }
        // A length-framed read whose opening was not known and that has met
        // nothing of its own knows no more than the record: what it would
        // note here is a guess, and the next read would take it for found.
        if !length || ((self.sure || self.met) && self.diverged) {
            if let (Some(at), Some(file)) = (at, self.file.as_ref()) {
                if self.head {
                    for b in std::mem::take(&mut self.before) {
                        remember(file, b, &self.record);
                    }
                }
                remember(file, at, &now);
            }
        }
        // Nor is a guess put in front of a key: the decoder keeps what it
        // holds, as it did before any of this was asked.
        if length && !self.sure && !self.met {
            return;
        }
        let needed = match &self.given {
            Some(given) => !same(&now, given),
            None => self.diverged || self.file.as_ref().is_some_and(diverges),
        };
        if !needed {
            return;
        }
        let data = packet.data().unwrap_or(&[]);
        let data = match self.framing {
            crate::bitstream::NalFraming::AnnexB => crate::cut::with_record_sets(data, &now),
            crate::bitstream::NalFraming::Length(n) => with_sets_in_lengths(data, &now, n),
        };
        let Some(data) = data else {
            return;
        };
        let mut out = ff::Packet::copy(&data);
        out.set_stream(packet.stream());
        out.set_pts(packet.pts());
        out.set_dts(packet.dts());
        out.set_duration(packet.duration());
        out.set_position(packet.position());
        out.set_flags(packet.flags());
        *packet = out;
        if let Some(given) = self.given.as_mut() {
            for s in &now {
                put_set(given, s);
            }
        }
    }
}

/// The SPS and PPS NAL units a packet restates.
///
/// Read only up to its first slice: an access unit states its sets ahead of
/// its pictures, and this is asked of every packet a read goes past -- the
/// thumbnail pass reads the whole recording -- so the slices, which are
/// nearly all of its bytes, are not searched for start codes.
///
/// In lengths, every NAL unit the packet holds, as `copy_segment`'s
/// `length_sets` reads them ([`crate::bitstream::nal_payloads`]): a length
/// steps over a slice without reading it.
pub(crate) fn restated(data: &[u8], framing: crate::bitstream::NalFraming) -> Vec<&[u8]> {
    if let crate::bitstream::NalFraming::Length(_) = framing {
        let mut nals = crate::bitstream::nal_payloads(data, framing);
        nals.retain(|nal| matches!(nal.first().map(|b| b & 0x1F), Some(7 | 8)));
        return nals;
    }
    let mut out = Vec::new();
    // Where each start code's NAL begins, until the first slice.
    let mut i = 0;
    let mut open: Option<usize> = None;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let begins = i + 3;
            if let Some(at) = open.take() {
                // The previous NAL ends at this start code, less the zero of
                // a four-byte one, as [`crate::bitstream::nal_payloads`]
                // cuts it.
                let end = if i > at && data[i - 1] == 0 { i - 1 } else { i };
                if at < end {
                    out.push(&data[at..end]);
                }
            }
            match data.get(begins).map(|b| b & 0x1F) {
                Some(7 | 8) => open = Some(begins),
                Some(1..=5) | None => return out,
                _ => {}
            }
            i = begins;
        } else {
            i += 1;
        }
    }
    if let Some(at) = open {
        out.push(&data[at..]);
    }
    out
}

/// A length-framed H.264 key picture with `sets` put in front of its slices,
/// after its access unit delimiter, each in a length of `n` bytes: what
/// [`crate::cut::with_record_sets`] does in start codes, and `None` where it
/// says `None` (the packet carries sets of its own, or its first slice names
/// a picture set `sets` does not hold) or a set is too long for `n` bytes.
fn with_sets_in_lengths(data: &[u8], sets: &[Vec<u8>], n: usize) -> Option<Vec<u8>> {
    let kind = |nal: &[u8]| nal.first().map(|b| b & 0x1F);
    let nals = crate::bitstream::nal_payloads(data, crate::bitstream::NalFraming::Length(n));
    if nals.iter().any(|nal| matches!(kind(nal), Some(7 | 8))) {
        return None;
    }
    let slice = nals.iter().find(|nal| matches!(kind(nal), Some(1 | 5)))?;
    // first_mb_in_slice, slice_type, then the picture set's number.
    let mut b = crate::bitstream::Bits::new(slice);
    b.skip(8)?;
    b.ue()?;
    b.ue()?;
    let pps = b.ue()?;
    if !sets.iter().any(|s| set_key(s) == (Some(8), Some(pps))) {
        return None;
    }
    // After the delimiter, where the access unit opens with one.
    let at = match nals.first() {
        Some(aud) if kind(aud) == Some(9) => aud.as_ptr() as usize - data.as_ptr() as usize + aud.len(),
        _ => 0,
    };
    if n == 0 || n > 8 || sets.iter().any(|s| n < 8 && s.len() >> (8 * n) != 0) {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() + sets.iter().map(|s| s.len() + n).sum::<usize>());
    out.extend_from_slice(&data[..at]);
    // Sequence sets first: a picture set is read against its sequence set.
    let sequence = sets.iter().filter(|s| kind(s) == Some(7));
    for s in sequence.chain(sets.iter().filter(|s| kind(s) == Some(8))) {
        out.extend_from_slice(&(s.len() as u64).to_be_bytes()[8 - n..]);
        out.extend_from_slice(s);
    }
    out.extend_from_slice(&data[at..]);
    Some(out)
}

/// Whether a read of any MP4 or Matroska H.264 file in this process has met
/// a set in band that is not its record's. Until one has, [`InForce`] asks
/// nothing for such files.
static LENGTH_DIVERGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The kind and number an SPS or PPS is filed under, as a decoder files it.
pub(crate) fn set_key(nal: &[u8]) -> (Option<u8>, Option<u32>) {
    let kind = nal.first().map(|b| b & 0x1F);
    let mut b = crate::bitstream::Bits::new(nal);
    let id = b.skip(8).and_then(|_| match kind {
        Some(7) => b.skip(24).and_then(|_| b.ue()),
        Some(8) => b.ue(),
        _ => None,
    });
    (kind, id)
}

/// `sets` with `nal` in place of the set of its kind and number.
fn put_set(sets: &mut Sets, nal: &[u8]) {
    let key = set_key(nal);
    match sets.iter_mut().find(|s| set_key(s) == key) {
        Some(s) => {
            if s.as_slice() != nal {
                *s = nal.to_vec();
            }
        }
        None => sets.push(nal.to_vec()),
    }
}

/// The same sets, in whatever order.
fn same(a: &Sets, b: &Sets) -> bool {
    let mut a: Vec<&Vec<u8>> = a.iter().collect();
    let mut b: Vec<&Vec<u8>> = b.iter().collect();
    a.sort();
    b.sort();
    a == b
}

/// Which file [`sets_before`] answered for: its name, length and time,
/// and the record's sets.
pub(crate) type FileKey = (String, u64, Option<std::time::SystemTime>, Sets);

/// What [`sets_before`] has found, by file ([`FileKey`]): whether anything in
/// force anywhere in it is not the record's, and by byte the sets in force
/// in front of it.
#[allow(clippy::type_complexity)]
static SETS_KNOWN: std::sync::Mutex<Vec<(FileKey, bool, std::collections::BTreeMap<u64, Sets>)>> =
    std::sync::Mutex::new(Vec::new());

fn file_key(src: &Source, record: &[Vec<u8>]) -> FileKey {
    let meta = std::fs::metadata(&src.path).ok();
    (
        src.input.url.clone(),
        meta.as_ref().map_or(0, |m| m.len()),
        meta.and_then(|m| m.modified().ok()),
        record.to_vec(),
    )
}

/// The H.264 sets in force in front of byte `at` of `src`, starting from the
/// record's: [`crate::cut::sets_in_force`], read back no further than the
/// nearest byte before `at` already answered for the same file -- as the
/// cut reads no further back than its last range ([`crate::cut`]'s
/// `sets_at_key`). The film strip asks once per run of cells and the scene
/// pass once per read; without this, a remux whose sets are stated only at
/// its head read up to 32 MiB back for every cell.
pub(crate) fn sets_before(src: &Source, at: u64, record: &[Vec<u8>]) -> (FileKey, Sets) {
    let key = file_key(src, record);
    let near = SETS_KNOWN.lock().ok().and_then(|known| {
        known
            .iter()
            .find(|(k, _, _)| *k == key)
            .and_then(|(_, _, by)| by.range(..=at).next_back().map(|(p, s)| (*p, s.clone())))
    });
    if let crate::bitstream::NalFraming::Length(_) = src.video.framing {
        return (key.clone(), length_sets_before(src, &key, at, near, record));
    }
    let sets = match near {
        Some((p, s)) if p == at => return (key, s),
        Some((p, s)) => crate::cut::sets_in_force(src, at, p, &s),
        None => crate::cut::sets_in_force(src, at, 0, record),
    };
    remember(&key, at, &sets);
    (key, sets)
}

/// [`sets_before`] for an MP4 or Matroska file, which cannot be read back by
/// bytes: what is in force at the key picture at byte `at` is read back key
/// picture by key picture, as the cut reads it for a copy
/// ([`crate::cut::length_sets_at`]), each key picture once per recording.
/// Where `at` is no key picture of `src`'s index, what an earlier read found
/// nearest before it (`near`), else the record -- a guess, not noted (see
/// [`known_at`]).
fn length_sets_before(
    src: &Source,
    key: &FileKey,
    at: u64,
    near: Option<(u64, Sets)>,
    record: &[Vec<u8>],
) -> Sets {
    if let Some((p, s)) = &near {
        if *p == at {
            return s.clone();
        }
    }
    let Some(point) = src.points.iter().find(|p| u64::try_from(p.pos).ok() == Some(at)) else {
        return near.map_or_else(|| record.to_vec(), |(_, s)| s);
    };
    // What the process has read of the file's key pictures, kept by its name,
    // length and time of writing (see [`crate::cut::length_sets_known`]).
    let sets = crate::cut::length_sets_known(src, point.time, Some(record)).map_or_else(|| record.to_vec(), |(s, _)| s);
    remember(key, at, &sets);
    sets
}

/// Note what is in force in front of byte `at` of the file `key` names.
fn remember(key: &FileKey, at: u64, sets: &Sets) {
    let Ok(mut known) = SETS_KNOWN.lock() else { return };
    let i = match known.iter().position(|(k, _, _)| k == key) {
        Some(i) => i,
        None => {
            if known.len() >= 8 {
                known.remove(0);
            }
            known.push((key.clone(), false, Default::default()));
            known.len() - 1
        }
    };
    let (_, diverged, by) = &mut known[i];
    *diverged |= !same(sets, &key.3);
    if by.len() >= 1 << 14 {
        by.clear();
    }
    by.insert(at, sets.clone());
}

/// Whether an earlier read noted what is in force at byte `at` of the file
/// `key` names: a read in file order notes every key picture it passes once
/// anything has diverged, so a read that opens on one of those knows. One
/// noted further back may have a restatement between it and `at` that no
/// read here can see.
fn known_at(key: &FileKey, at: u64) -> bool {
    SETS_KNOWN
        .lock()
        .ok()
        .is_some_and(|known| known.iter().any(|(k, _, by)| k == key && by.contains_key(&at)))
}

/// Whether anything found in force in the file `key` names is not its
/// record's: then a pool's decoders, any of which may have met it, are
/// given the sets on every key picture that carries none.
fn diverges(key: &FileKey) -> bool {
    SETS_KNOWN
        .lock()
        .ok()
        .is_some_and(|known| known.iter().any(|(k, d, _)| k == key && *d))
}
/// Did decoding begin late enough to have missed the picture wanted?
fn landed_late(first: Option<f64>, wanted: f64, slack: f64) -> bool {
    !matches!(first, Some(f) if f <= wanted + slack)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_are_read_up_to_the_first_slice_as_nal_payloads_cuts_them() {
        let sps0: Vec<u8> = vec![0x67, 0x64, 0x00, 0x1e, 0xac, 0xd9];
        let pps0: Vec<u8> = vec![0x68, 0xeb, 0xe3, 0xcb];
        let pps2: Vec<u8> = vec![0x68, 0x6b, 0xe3, 0xcb];
        // AUD, SPS (four-byte start code), PPS, slice, and a PPS after the
        // slice that is not looked for.
        let au = [
            &[0u8, 0, 0, 1, 0x09, 0x10][..],
            &[0, 0, 0, 1],
            &sps0,
            &[0, 0, 1],
            &pps0,
            &[0, 0, 1, 0x01, 0x88, 0x80],
            &[0, 0, 1],
            &pps2,
        ]
        .concat();
        assert_eq!(restated(&au, crate::bitstream::NalFraming::AnnexB), vec![&sps0[..], &pps0[..]]);
        let all = crate::bitstream::nal_payloads(&au, crate::bitstream::NalFraming::AnnexB);
        assert_eq!(all[1], &sps0[..]);
        assert_eq!(all[2], &pps0[..]);
        assert!(restated(&[0, 0, 1, 0x65, 0x88, 0, 0, 1, 0x67, 1], crate::bitstream::NalFraming::AnnexB).is_empty());
        assert_eq!(restated(&[0, 0, 1, 0x68, 0xeb], crate::bitstream::NalFraming::AnnexB), vec![&[0x68u8, 0xeb][..]]);
        // Filed by kind and number: PPS 2 beside PPS 0, a new PPS 0 in its place.
        let mut sets = vec![sps0.clone(), pps0.clone()];
        put_set(&mut sets, &pps2);
        assert_eq!(sets.len(), 3);
        let pps0b: Vec<u8> = vec![0x68, 0xee, 0x3c, 0x80];
        put_set(&mut sets, &pps0b);
        assert_eq!(sets, vec![sps0.clone(), pps0b.clone(), pps2.clone()]);
        assert!(same(&sets, &vec![pps2, sps0, pps0b]));
    }

    #[test]
    fn sets_in_lengths_are_read_and_put_after_the_delimiter() {
        use crate::bitstream::NalFraming;
        let sps0: Vec<u8> = vec![0x67, 0x64, 0x00, 0x1e, 0xac, 0xd9];
        let pps0: Vec<u8> = vec![0x68, 0xeb, 0xe3, 0xcb];
        let pps2: Vec<u8> = vec![0x68, 0x6b, 0xe3, 0xcb];
        let len = |n: &[u8]| [&(n.len() as u32).to_be_bytes()[..], n].concat();
        let aud = [0x09u8, 0x10];
        let slice = [0x65u8, 0x88, 0x80];
        // A key with its own sets: read, and left as it is.
        let own = [len(&aud), len(&sps0), len(&pps0), len(&slice)].concat();
        assert_eq!(restated(&own, NalFraming::Length(4)), vec![&sps0[..], &pps0[..]]);
        assert_eq!(with_sets_in_lengths(&own, &[sps0.clone(), pps0.clone()], 4), None);
        // A key without: the sets go in after the delimiter, SPS first.
        let bare = [len(&aud), len(&slice)].concat();
        assert!(restated(&bare, NalFraming::Length(4)).is_empty());
        let put = with_sets_in_lengths(&bare, &[pps0.clone(), sps0.clone()], 4).unwrap();
        assert_eq!(put, own);
        // Without a delimiter, in front.
        let put = with_sets_in_lengths(&len(&slice), &[sps0.clone(), pps0.clone()], 4).unwrap();
        assert_eq!(put, [len(&sps0), len(&pps0), len(&slice)].concat());
        // The slice names PPS 0, which these sets do not hold.
        assert_eq!(with_sets_in_lengths(&bare, &[sps0.clone(), pps2], 4), None);
        // Two-byte lengths.
        let len2 = |n: &[u8]| [&(n.len() as u16).to_be_bytes()[..], n].concat();
        let put = with_sets_in_lengths(&len2(&slice), &[sps0.clone(), pps0.clone()], 2).unwrap();
        assert_eq!(put, [len2(&sps0), len2(&pps0), len2(&slice)].concat());
    }

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
        let got = slots.finish(|n| Ok(vec![n as u8])).unwrap();
        assert_eq!(got.len(), 4);
        let at: Vec<Option<f64>> = got.iter().map(|s| s.as_ref().map(|s| s.time)).collect();
        assert_eq!(at, vec![Some(1.02), None, Some(2.05), None]);
        assert_eq!(got[0].as_ref().unwrap().jpeg, vec![2]);
        // Two pictures a hair apart are two pictures: a last one 65 us after
        // the one before keeps the cell it is nearest.
        let wanted = [19.9333, 19.9667];
        let mut slots: Slots<'_, u32> = Slots::new(&wanted, fd);
        for (n, t) in [19.933333, 19.933398].into_iter().enumerate() {
            slots.offer(t, "P", || Ok(n as u32)).unwrap();
        }
        let at: Vec<Option<f64>> =
            slots.finish(|n| Ok(vec![n as u8])).unwrap().iter().map(|s| s.as_ref().map(|s| s.time)).collect();
        assert_eq!(at, vec![Some(19.933333), Some(19.933398)]);
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

    /// The shapes a read can meet a seam in besides walking up to it: a seek
    /// that lands past it, two seams on the same byte, a packet exactly on
    /// the seam's byte, and a stretch a mend has emptied -- its pictures end
    /// before it begins, so none of them is the range's, as the plan has it
    /// (`at_the_seams` keeps no piece there).
    #[test]
    fn stretches_at_their_edges() {
        use crate::restamp::Seam;
        let fd = 1.0 / 25.0;
        let joins = [
            Seam { at: 100, time: 10.0, ends: 10.0 },
            Seam { at: 200, time: 20.0, ends: 20.0 },
            // Stretch 2's pictures were mended to end at 15 s, before it
            // begins at 20 s.
            Seam { at: 300, time: 30.0, ends: 15.0 },
        ];
        // A seek straight into stretch 1: the seam before it is passed with
        // the first packet, and the stop is the next seam's.
        let mut s = Stretches::of(&joins, fd);
        s.reach(150);
        assert!(s.outside(9.0) && !s.outside(10.0) && !s.outside(19.9) && s.outside(20.0));
        // On the seam's own byte is the stretch after it.
        let mut s = Stretches::of(&joins, fd);
        s.reach(99);
        assert!(!s.outside(9.99) && s.outside(10.0));
        s.reach(100);
        assert!(!s.outside(10.0) && s.outside(9.9));
        // The emptied stretch holds nothing the range writes.
        s.reach(250);
        for t in [14.0, 15.0, 20.0, 25.0, 29.9] {
            assert!(s.outside(t), "{t}");
        }
        // And past it, the last stretch runs on to the end.
        s.reach(300);
        assert!(!s.outside(30.0) && !s.outside(1e6) && s.outside(29.0));
        // Whether a read crossed a seam, which is when a reader that decodes
        // drains what it holds of the stretch before: only on the packet
        // that takes it over one, never on one with no position, and never
        // on a recording without seams.
        let mut s = Stretches::of(&joins, fd);
        assert!(!s.reach(50) && !s.reach(99) && !s.reach(-1));
        assert!(s.reach(100) && !s.reach(100) && !s.reach(150));
        assert!(s.reach(5000) && !s.reach(-1) && !s.reach(6000));
        let mut s = Stretches::of(&[], fd);
        assert!(!s.reach(0) && !s.reach(1 << 40));
        // Two seams on one byte: the read is in the later one's stretch.
        let twins = [
            Seam { at: 100, time: 10.0, ends: 10.0 },
            Seam { at: 100, time: 12.0, ends: 12.0 },
        ];
        let mut s = Stretches::of(&twins, fd);
        s.reach(100);
        assert!(s.outside(11.0) && !s.outside(12.0) && !s.outside(50.0));
    }

    /// BT.709 red, white and grey come out as BT.601 has them: what a JPEG
    /// decoder makes of the samples is the colour the recording is.
    #[test]
    fn a_jpeg_is_given_bt601_samples() {
        crate::init().unwrap();
        let mut f = ff::frame::Video::new(ff::format::Pixel::YUVJ420P, 4, 2);
        // Left block red as BT.709 codes it, right block white.
        let (ys, cs) = (f.stride(0), f.stride(1));
        for y in 0..2 {
            f.data_mut(0)[y * ys..y * ys + 2].fill(54);
            f.data_mut(0)[y * ys + 2..y * ys + 4].fill(255);
        }
        f.data_mut(1)[..2].copy_from_slice(&[99, 128]);
        f.data_mut(2)[..2].copy_from_slice(&[255, 128]);
        let _ = cs;
        rematrix(&mut f, (0.2126, 0.0722));
        let y = f.data(0)[0];
        let (cb, cr) = (f.data(1)[0], f.data(2)[0]);
        // BT.601 red: 76, 85, 255.
        assert!((i32::from(y) - 76).abs() <= 1 && (i32::from(cb) - 85).abs() <= 1 && cr >= 254, "{y} {cb} {cr}");
        assert_eq!((f.data(0)[2], f.data(1)[1], f.data(2)[1]), (255, 128, 128));
    }

    /// A lone second field joined in front of an entry picture's headers is
    /// found and left behind; a packet that opens on them, one with no I
    /// picture, and the headers of a B picture are left as they are.
    #[test]
    fn an_entry_picture_behind_a_stray_field() {
        let field_b = [0, 0, 1, 0x00, 0x12, 0x18, 0, 0, 0, 0, 1, 0xB5, 0x8F, 0, 0, 1, 0x01, 0xAA, 0xBB];
        let seq = [0, 0, 1, 0xB3, 0x5A, 0x04, 0x38, 0x33];
        let gop = [0, 0, 1, 0xB8, 0, 8, 0, 0];
        let i_pic = [0, 0, 1, 0x00, 0x00, 0x08, 0xFF, 0xF8, 0, 0, 1, 0x01, 0x11];
        let p_pic = [0, 0, 1, 0x00, 0x00, 0x50, 0, 0, 0, 0, 1, 0x01, 0x22];
        let join = |parts: &[&[u8]]| parts.concat();
        let fl = field_b.len();
        assert_eq!(entry_start(&join(&[&field_b, &seq, &gop, &i_pic, &p_pic])), Some(fl));
        assert_eq!(entry_start(&join(&[&field_b, &gop, &i_pic])), Some(fl));
        assert_eq!(entry_start(&join(&[&field_b, &i_pic, &seq])), Some(fl));
        assert_eq!(entry_start(&join(&[&seq, &gop, &i_pic])), None);
        assert_eq!(entry_start(&join(&[&field_b, &p_pic])), None);
        assert_eq!(entry_start(&[0, 0, 1]), None);
        assert_eq!(entry_start(&[]), None);
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
