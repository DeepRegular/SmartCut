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

use anyhow::{anyhow, bail, Result};
use ffmpeg_next as ff;

use crate::Source;

pub struct ThumbOptions {
    /// Shortest spacing between kept thumbnails. Every key picture is
    /// compared, but only pictures this far apart are held as images.
    ///
    /// A floor, not the spacing. What is kept are key pictures, and a
    /// recording puts those where it likes, so the pictures land wherever
    /// the material's own entry points are and the floor is merely what
    /// they are never closer together than. What they turned out to be is
    /// [`Track::interval`], and that is the number a caller wants.
    ///
    /// Zero means "keep every key picture", which is what a scroll search
    /// wants: dragging across the film strip shows the held pictures, so
    /// their spacing is the granularity of the scroll.
    pub interval: f64,
    /// Ceiling on what the held pictures may come to, in bytes, whatever
    /// `interval` says. A recording whose entry points would not fit widens
    /// its own spacing rather than filling memory.
    ///
    /// **In bytes rather than in pictures**, which is what it used to be. The
    /// two are the same thing only where a picture is always the same size,
    /// and a 192-pixel JPEG runs from 3 KB on a disc's clean 1080p to 8 KB on
    /// a noisy broadcast. Counted in pictures the ceiling stopped being about
    /// memory and became about *length*: 4000 of them is a floor of the
    /// recording's length over 4000, which is nothing on half an hour and
    /// 2.1 s on a 2 h 20 m film -- coarser than the film strip's own cells,
    /// so every cell of it missed the track and was decoded out of the
    /// recording instead. Measured on a VC-1 disc that was every refresh, at
    /// a quarter to three quarters of a second each.
    ///
    /// What the number is worth is therefore read off the pictures as they
    /// are made; see [`Collector::min_gap`].
    pub max_bytes: usize,
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
    /// How many cores the decoder may have. Zero, the default, is all of
    /// them.
    ///
    /// Said only by the clip list, whose passes run behind an open cut
    /// editor and must leave it something to decode with. See
    /// [`crate::video_decoder_with`].
    pub threads: usize,
}

impl Default for ThumbOptions {
    fn default() -> Self {
        Self {
            interval: 0.0,
            // Enough that every entry point of anything anyone opens here is
            // held: the longest recording in question is a disc title, and
            // three hours of one carries around 20 000 of them. Past that the
            // spacing widens, which is the old behaviour and the right one --
            // it is simply no longer reached by an ordinary film.
            max_bytes: 256 << 20,
            width: 192,
            floor: 0.055,
            over_typical: 3.0,
            min_spacing: 3.0,
            threads: 0,
        }
    }
}

pub struct Thumb {
    pub time: f64,
    pub jpeg: Vec<u8>,
}

pub struct Track {
    pub width: u32,
    /// The spacing the held pictures actually sit at -- the median gap
    /// between neighbours -- which is what a caller must assume when
    /// deciding whether they are fine enough for its purpose.
    ///
    /// **Measured, not asked for.** [`ThumbOptions::interval`] and the budget
    /// they have to fit in only set a floor -- usually none at all; what is
    /// held are key pictures, so the spacing is the material's. On a 2 m 46 s
    /// BS Fuji recording the floor is zero and the pictures land 0.50 s apart
    /// -- and a caller told the floor would answer a scrub from held pictures
    /// that are nowhere near fine enough for it.
    pub interval: f64,
    /// How far into the recording the track speaks for. A finished one speaks
    /// for the whole of it; one handed over while it is still being built
    /// speaks only for what has been decoded, and the caller has to stop
    /// asking past here -- `nearest` is happy to answer with the last picture
    /// it has, which is the wrong picture rather than no picture.
    pub covered: f64,
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

    /// The spacing the held pictures sit at *between `a` and `b`*, or `None`
    /// where there are fewer than two of them in that stretch to measure
    /// from.
    ///
    /// [`Self::interval`] is the same measurement over the whole recording,
    /// and on broadcast material the two are the same number: a GOP is half a
    /// second wherever you look. A disc is not like that. It puts an entry
    /// point at every scene change on top of one a second or so, so a minute
    /// of fast cutting holds pictures twice as thickly as a minute of
    /// dialogue, and the recording's median says nothing about either. A
    /// caller asking "are these fine enough to answer me with" is asking
    /// about the stretch it is drawing, so it is that stretch that has to
    /// answer.
    ///
    /// A stretch that is not one -- backwards, or either end not a number --
    /// is nothing to measure rather than a slice to panic on.
    pub fn spacing_over(&self, a: f64, b: f64) -> Option<f64> {
        if !(a <= b) {
            return None;
        }
        let i = self.thumbs.partition_point(|t| t.time < a - 1e-6);
        let j = self.thumbs.partition_point(|t| t.time <= b + 1e-6);
        spacing(&self.thumbs[i..j])
    }

    pub fn scene_after(&self, time: f64) -> Option<f64> {
        self.scenes.iter().copied().find(|&s| s > time + 1e-3)
    }

    pub fn scene_before(&self, time: f64) -> Option<f64> {
        self.scenes.iter().rev().copied().find(|&s| s < time - 1e-3)
    }
}

/// The spacing a run of held pictures sits at, or `None` when there are
/// fewer than two of them and nothing can be told.
pub fn spacing(thumbs: &[Thumb]) -> Option<f64> {
    median_gap(&thumbs.windows(2).map(|w| w[1].time - w[0].time).collect::<Vec<_>>())
}

/// What a run of gaps amounts to as a spacing: their median.
///
/// The median rather than the mean, or the smallest. Key pictures are not
/// evenly spaced -- a recording carries one every half second and then an
/// extra one wherever the picture changes enough to be worth it, and a
/// damaged or abandoned stretch leaves a gap of seconds. The mean is dragged
/// up by the long gaps and the minimum is decided by a single short one;
/// neither is the spacing anything on screen actually sees. On a BS Fuji
/// recording the gaps run from 0.27 s to 6.5 s and the median is 0.50 s,
/// which is what the film strip is really drawn at.
///
/// Shared with the callers who have to weigh a spacing they are *asking*
/// for against the one the pictures sit at, so that the two numbers are
/// arrived at the same way.
pub fn median_gap(gaps: &[f64]) -> Option<f64> {
    if gaps.is_empty() {
        return None;
    }
    let mut sorted = gaps.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(sorted[sorted.len() / 2])
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

/// Somewhere to hand key pictures as they come out of a decoder.
///
/// Split out from [`build`] because there are two ways to arrive at the same
/// pictures. Building the proxy decodes the whole recording anyway and knows
/// which pictures are its access points, so it feeds them straight in and the
/// track costs nothing beyond the JPEGs; opening a recording that already has
/// a proxy runs [`build`] over the proxy instead. Either way the track is
/// made the same, which is what lets the two be used interchangeably.
pub struct Collector<'a> {
    opts: &'a ThumbOptions,
    /// The recording's sample aspect, applied when a picture is encoded so
    /// that a 4:3 recording comes back 4:3 whatever its coded width says.
    sar: f64,
    /// How long the recording is, as well as it is known -- which at the head
    /// of a one-pass read is only what the container claimed. See
    /// [`Self::min_gap`].
    duration: f64,
    /// The gap between each kept picture and the one before it, which is
    /// what the spacing is measured from. Kept here rather than read back
    /// off the pictures because [`Self::take_new`] moves those out.
    gaps: Vec<f64>,
    /// The time of the last picture kept, for the gap to the next one.
    last_kept: Option<f64>,
    thumbs: Vec<Thumb>,
    diffs: Vec<(f64, f64)>,
    prev: Option<[u8; SIG]>,
    keep_from: f64,
    /// The last time fed in, which is how far the pictures held so far
    /// speak for.
    seen: f64,
    /// How many key pictures have been offered, and how many of those were
    /// kept and what they came to. The first is the recording's own rate of
    /// entry points and the other two are what one of them weighs; between
    /// them they say whether holding all of them would fit. See
    /// [`Self::min_gap`].
    ///
    /// Counted here rather than read off `thumbs`, which [`Self::take_new`]
    /// moves out from under them.
    fed: usize,
    kept: usize,
    bytes: usize,
}

/// What one held picture is assumed to weigh until some have been made.
///
/// Only the first second or so of a read goes by on this, and what it decides
/// is whether that second is held finely or not at all -- so it is set where
/// a wrong guess is harmless: broadcast material, the heavier of the two.
const SEED_BYTES: usize = 8 << 10;

impl<'a> Collector<'a> {
    pub fn new(src: &'a Source, opts: &'a ThumbOptions) -> Self {
        Self::about(src.duration, src.video.sample_aspect_ratio, opts)
    }

    /// As [`Self::new`], for a caller that has no [`Source`] yet.
    ///
    /// The one-pass read has none: the index is what a `Source` is mostly
    /// made of, and it does not exist until the read that this is collecting
    /// during has finished. What the collector actually wants of a recording
    /// is these two numbers. See [`crate::scan_with_pictures`].
    pub fn about(duration: f64, sar: f64, opts: &'a ThumbOptions) -> Self {
        Self {
            opts,
            sar,
            duration,
            gaps: Vec::new(),
            last_kept: None,
            thumbs: Vec::new(),
            diffs: Vec::new(),
            prev: None,
            keep_from: f64::NEG_INFINITY,
            seen: 0.0,
            fed: 0,
            kept: 0,
            bytes: 0,
        }
    }

    /// What one held picture weighs, as well as it is known.
    fn each(&self) -> f64 {
        if self.kept == 0 {
            SEED_BYTES as f64
        } else {
            (self.bytes as f64 / self.kept as f64).max(1.0)
        }
    }

    /// Nothing is kept closer together than this. A floor only -- see
    /// [`Track::interval`] for why it is not the answer to "how far apart
    /// are the pictures".
    ///
    /// Worked out afresh each time rather than once, because none of the three
    /// numbers it rests on is known at the start. The length may still be
    /// wrong -- a program stream does not record its own, and libavformat has
    /// been seen to work out eight seconds for an hour of DVD -- so the longer
    /// of what the container claimed and what has actually gone past is used,
    /// which makes the floor correct itself as the read goes on: the first few
    /// seconds are held too finely and everything after them is held right.
    /// What a picture weighs and how thickly the recording carries entry
    /// points are likewise only learnt by reading it.
    ///
    /// **Nothing is thinned at all where holding all of it would fit.** That
    /// is the case worth getting right, because it is the one the film strip
    /// lives in: its cells stand on the recording's own entry points, so a
    /// track missing any of them is a strip that has to decode. Only once the
    /// budget will not stretch to every entry point does a floor appear, and
    /// then it spreads what is left of the budget over what is left of the
    /// recording.
    ///
    /// The projection counts the pictures *offered* rather than the ones kept.
    /// Thinning lowers the rate they are kept at, so a projection made from
    /// that would report the budget safe again the moment it began thinning,
    /// and the floor would come and go by turns.
    fn min_gap(&self) -> f64 {
        let known = self.duration.max(self.seen);
        let each = self.each();
        // Too early to have measured a rate; the first second is held whole.
        if self.fed < 2 || self.seen < 1.0 {
            return self.opts.interval;
        }
        let entry_points = self.fed as f64 * (known / self.seen);
        if entry_points * each <= self.opts.max_bytes as f64 {
            return self.opts.interval;
        }
        let left = self.opts.max_bytes.saturating_sub(self.bytes) as f64;
        let ahead = (known - self.seen).max(0.0);
        self.opts.interval.max(ahead / (left / each).max(1.0))
    }

    /// The spacing images are actually being kept at, falling back to the
    /// floor while there are too few of them to have measured anything.
    pub fn interval(&self) -> f64 {
        median_gap(&self.gaps).unwrap_or(self.min_gap())
    }

    /// The pictures collected since this was last called.
    ///
    /// For handing a track over while it is still being made: the film strip
    /// and the scroll search want held pictures long before the pass that is
    /// producing them has reached the end of the file. Taken rather than
    /// copied, so the pictures are not held twice -- what [`Self::finish`]
    /// returns is then only the tail, and whoever has been taking the rest
    /// owns them and must put the two back together.
    pub fn take_new(&mut self) -> Batch {
        Batch {
            thumbs: std::mem::take(&mut self.thumbs),
            width: self.opts.width,
            interval: self.interval(),
            covered: self.seen,
        }
    }

    /// Offer a key picture, presented at `time`.
    pub fn feed(&mut self, time: f64, frame: &ff::frame::Video) -> Result<()> {
        if time < -1.0 {
            return Ok(());
        }
        self.seen = time;
        self.fed += 1;
        let sig = signature(frame);
        if let Some(p) = &self.prev {
            self.diffs.push((time, distance(p, &sig)));
        }
        self.prev = Some(sig);

        if time >= self.keep_from {
            let jpeg = crate::preview::encode_jpeg(frame, self.sar, self.opts.width)?;
            if let Some(prev) = self.last_kept {
                self.gaps.push(time - prev);
            }
            self.last_kept = Some(time);
            self.kept += 1;
            self.bytes += jpeg.len();
            // After the picture is counted, not before: what the floor is
            // worth depends on what the pictures have turned out to weigh,
            // and this one is now part of that answer.
            self.keep_from = time + self.min_gap();
            self.thumbs.push(Thumb { time, jpeg });
        }
        Ok(())
    }

    /// The finished track. `duration` is the recording's real length, which
    /// the one-pass read only learns when the read is over -- see
    /// [`index::Index::end`] -- and which is what the scene marks are spaced
    /// against.
    pub fn finish(self, duration: f64) -> Track {
        let interval = self.interval();
        let (scenes, threshold, typical) = mark_scenes(&self.diffs, duration, self.opts);
        Track {
            width: self.opts.width,
            interval,
            // The pass is over, so the track speaks for the whole recording
            // -- including any tail with no key picture in it at all.
            covered: f64::INFINITY,
            thumbs: self.thumbs,
            scenes,
            threshold,
            typical,
        }
    }
}

/// Pictures collected since the last hand-over, and what a track made of them
/// would have to say about itself.
pub struct Batch {
    pub thumbs: Vec<Thumb>,
    pub width: u32,
    /// The spacing measured over everything kept so far, not just over
    /// these: the pictures already handed over are the same pass and the
    /// same material, and a whole run is a steadier measurement than a
    /// half-second of one.
    pub interval: f64,
    /// How far into the recording the pass producing these has read.
    pub covered: f64,
}

impl Batch {
    /// A track holding just these.
    ///
    /// It speaks only for what has been read so far, and it has no scene
    /// index: a scene is a difference between two key pictures measured
    /// against what is typical of the whole recording, and that is not known
    /// until the pass ends.
    pub fn into_track(self) -> Track {
        Track {
            width: self.width,
            interval: self.interval,
            covered: self.covered,
            thumbs: self.thumbs,
            scenes: Vec::new(),
            threshold: 0.0,
            typical: 0.0,
        }
    }
}

/// How often the pictures collected so far are handed to a `share` callback.
///
/// The point of handing them over at all is that whatever is waiting for the
/// track -- the film strip, the scroll search, the mark cards -- stops having
/// to decode the recording, so this wants to be short. It is a `Vec` moved
/// across a callback, so short costs nothing.
pub const SHARE_EVERY: std::time::Duration = std::time::Duration::from_millis(500);

/// Decode every key picture once: keep some as images, compare them all.
pub fn build(
    src: &Source,
    opts: &ThumbOptions,
    progress: Option<Box<dyn FnMut(f64) + Send>>,
) -> Result<Track> {
    build_with(src, opts, progress, None, None)
}

/// As [`build`], but handing the pictures over as they are made and stopping
/// when asked.
///
/// The pass runs for around ten seconds on half an hour of 1440x1080 MPEG-2,
/// and until it finishes there is nothing but the recording to answer the
/// film strip from -- a seek and a GOP for every cell. These are the same
/// pictures, decoded in order from the start, so the part already gone past
/// answers instantly while the rest is still being read. **They are moved,
/// not copied**: what comes back then holds only the tail, and a caller that
/// took some owns them and must put the two halves back together.
///
/// `stop` is asked between packets; answering true abandons the pass.
pub fn build_with(
    src: &Source,
    opts: &ThumbOptions,
    mut progress: Option<Box<dyn FnMut(f64) + Send>>,
    mut share: Option<Box<dyn FnMut(Batch) + Send>>,
    stop: Option<Box<dyn Fn() -> bool + Send>>,
) -> Result<Track> {
    crate::init()?;
    let mut ictx = crate::input::demux(&src.input.url)?;
    let idx = src.video.stream_index;
    let params = ictx.stream(idx).ok_or_else(|| anyhow!("video stream vanished"))?.parameters();
    let mut decoder = crate::video_decoder_with(params, opts.threads)?;

    let mut collector = Collector::new(src, opts);
    let mut told = -1.0;
    let mut frame = ff::frame::Video::empty();
    let mut shared = std::time::Instant::now();

    let mut take = |frame: &ff::frame::Video| -> Result<()> {
        let Some(pts) = frame.pts() else { return Ok(()) };
        let t = pts as f64 * src.video.time_base - src.start_time;
        collector.feed(t, frame)?;
        if let Some(f) = progress.as_mut() {
            let done = (t / src.duration.max(1e-9)).clamp(0.0, 1.0);
            if done - told >= 0.01 {
                told = done;
                f(done);
            }
        }
        if let Some(f) = share.as_mut() {
            if shared.elapsed() >= SHARE_EVERY {
                shared = std::time::Instant::now();
                f(collector.take_new());
            }
        }
        Ok(())
    };

    for (stream, packet) in ictx.packets() {
        if let Some(f) = stop.as_ref() {
            if f() {
                bail!("abandoned");
            }
        }
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

    Ok(collector.finish(src.duration))
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

    let mut ictx = crate::input::demux(&src.input.url)?;
    let idx = src.video.stream_index;
    // Straight to the byte the entry point starts at where the index has it.
    // Failing that, a transport stream seeks by byte position of its own
    // reckoning and can land a GOP late, so aim early and read forward.
    if crate::index::seek_to_entry(&mut ictx, src, entry).is_none() {
        let landing = (entry - src.seek_margin).max(0.0);
        let target = ((landing + src.start_time) * ff::ffi::AV_TIME_BASE as f64) as i64;
        let _ = ictx.seek(target, ..target);
    }
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

    let mut ictx = crate::input::demux(&src.input.url)?;
    let idx = src.video.stream_index;
    // Straight to the byte the entry point starts at where the index has it;
    // otherwise start well before the picture wanted and read forward, since
    // a transport stream's own seeking can land a GOP late.
    if crate::index::seek_to_entry(&mut ictx, src, from).is_none() {
        let landing = (from - src.seek_margin).max(0.0);
        let target = ((landing + src.start_time) * ff::ffi::AV_TIME_BASE as f64) as i64;
        let _ = ictx.seek(target, ..target);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn at(times: &[f64]) -> Vec<Thumb> {
        times.iter().map(|&time| Thumb { time, jpeg: Vec::new() }).collect()
    }

    #[test]
    fn nothing_to_measure_says_so() {
        assert_eq!(spacing(&at(&[])), None);
        assert_eq!(spacing(&at(&[1.0])), None);
        assert_eq!(spacing(&at(&[1.0, 1.5])), Some(0.5));
    }

    /// The shape of a real recording: half a second between entry points,
    /// one short GOP where the picture changed, and a stretch with none at
    /// all. Neither outlier is allowed to become the answer -- the mean of
    /// these is 1.08 and the smallest is 0.27, and what the film strip is
    /// drawn at is 0.50.
    #[test]
    fn outliers_do_not_become_the_spacing() {
        let mut times = vec![0.0];
        for _ in 0..8 {
            times.push(times.last().unwrap() + 0.5);
        }
        times.push(times.last().unwrap() + 0.27);
        times.push(times.last().unwrap() + 6.5);
        let s = spacing(&at(&times)).unwrap();
        assert!((s - 0.5).abs() < 1e-9, "spacing came out {s}");
    }

    fn track_at(times: &[f64]) -> Track {
        Track {
            width: 192,
            interval: spacing(&at(times)).unwrap_or(0.0),
            covered: f64::INFINITY,
            thumbs: at(times),
            scenes: Vec::new(),
            threshold: 0.0,
            typical: 0.0,
        }
    }

    /// The shape a disc has and broadcast material does not: a stretch of
    /// fast cutting, where the entry points come more than twice as thickly
    /// as they do over the recording as a whole. The film strip draws that
    /// stretch out of pictures it holds every one of, so what it has to be
    /// told is the spacing *there* -- the recording's own median is a number
    /// about somewhere else.
    #[test]
    fn a_busy_stretch_is_measured_where_it_is() {
        let mut times = Vec::new();
        let mut t = 0.0;
        for _ in 0..20 {
            times.push(t);
            t += 1.0;
        }
        for _ in 0..20 {
            times.push(t);
            t += 0.4;
        }
        for _ in 0..20 {
            times.push(t);
            t += 1.0;
        }
        let track = track_at(&times);
        assert!((track.interval - 1.0).abs() < 1e-9, "over all it is {}", track.interval);
        let busy = track.spacing_over(22.0, 28.0).unwrap();
        assert!((busy - 0.4).abs() < 1e-9, "over the cutting it is {busy}");
        let quiet = track.spacing_over(2.0, 8.0).unwrap();
        assert!((quiet - 1.0).abs() < 1e-9, "over the dialogue it is {quiet}");
    }

    /// A stretch too short to hold two pictures cannot be measured, and says
    /// so rather than making a number up: the caller falls back on the
    /// recording's own, and a request that fine is one meant to be decoded.
    #[test]
    fn a_stretch_with_nothing_in_it_says_nothing() {
        let track = track_at(&[0.0, 1.0, 2.0, 3.0]);
        assert_eq!(track.spacing_over(1.1, 1.9), None);
        assert_eq!(track.spacing_over(0.9, 2.1), Some(1.0));
        assert_eq!(track.spacing_over(2.0, 1.0), None);
        assert_eq!(track.spacing_over(f64::NAN, 1.0), None);
    }

    /// A minute into a 2 h 20 m disc title: two entry points a second, six
    /// kilobytes a picture, so holding all of them comes to around a hundred
    /// megabytes.
    fn a_film<'a>(opts: &'a ThumbOptions) -> Collector<'a> {
        let mut c = Collector::about(8460.0, 1.0, opts);
        c.seen = 60.0;
        c.fed = 120;
        c.kept = 120;
        c.bytes = 120 * 6144;
        c
    }

    /// The point of the budget: a recording whose entry points fit inside it
    /// keeps every one of them, because every one is a cell the film strip
    /// can be asked for. The old ceiling was a count, and 4000 of them over a
    /// film is a floor of 2.1 s -- coarser than the strip's own cells, so
    /// every cell missed and was decoded out of the recording.
    #[test]
    fn a_recording_that_fits_is_not_thinned_at_all() {
        let opts = ThumbOptions::default();
        assert_eq!(a_film(&opts).min_gap(), 0.0);
    }

    /// And one that does not fit spreads what is left of the budget over what
    /// is left of the recording, rather than over the whole of it: the part
    /// already read has been paid for.
    #[test]
    fn a_recording_that_does_not_fit_spreads_what_is_left() {
        let opts = ThumbOptions { max_bytes: 8 << 20, ..ThumbOptions::default() };
        let c = a_film(&opts);
        let room = ((8 << 20) - 120 * 6144) as f64 / 6144.0;
        let want = (8460.0 - 60.0) / room;
        let got = c.min_gap();
        assert!((got - want).abs() < 1e-9, "floor came out {got}, wanted {want}");
    }

    /// Nothing is dropped before there is anything to judge on. A rate read
    /// off one picture is not a rate.
    #[test]
    fn the_first_moment_is_held_whole() {
        let opts = ThumbOptions { max_bytes: 1 << 10, ..ThumbOptions::default() };
        let mut c = Collector::about(8460.0, 1.0, &opts);
        assert_eq!(c.min_gap(), 0.0);
        c.seen = 0.5;
        c.fed = 2;
        assert_eq!(c.min_gap(), 0.0);
    }
}
