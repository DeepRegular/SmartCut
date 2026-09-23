//! Finding the station logo, and the stretches where it is missing.
//!
//! Japanese broadcasters keep a small translucent logo in one corner during
//! the programme and take it down for the commercials. That makes it a
//! second, independent read on where a break is -- one that does not care
//! whether the junctions happened to be silent.
//!
//! No logo library is needed. The logo is the only thing in its corner that
//! never moves, so averaging a few thousand frames leaves it standing while
//! the pictures behind it blur away; high-pass filtering that average is the
//! template. Scoring a moment is then a correlation against it.
//!
//! **What this costs is now the reading, not the decoding.** The pass makes
//! two walks over the entry pictures of the whole recording, and both of them
//! used to run in one decoder libavcodec was never told it could thread: on a
//! 30-minute terrestrial recording held in memory that was 6.9 seconds, and
//! it is 1.5 now. What is left is the file itself. The same recording off the
//! disc takes 46 seconds, of which 42 is two reads of 3.7 GB, and every
//! measurement of this pass on material that does not fit in memory is a
//! measurement of the drive it sits on.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;

use crate::Source;

pub struct LogoOptions {
    /// Correlation above which the logo counts as present.
    pub present: f64,
    /// ...and below which it counts as gone. The gap is hysteresis.
    pub absent: f64,
    /// Ignore absences shorter than this; a commercial break never is.
    pub min_absent: f64,
    /// ...but a channel's own animated ident is, and a subscription channel
    /// drops one into a programme where the terrestrial broadcast had its
    /// commercials. An absence at least this long is reported separately as
    /// [`Logo::brief`], for the caller to keep where a junction stands in
    /// it; see [`crate::cm::blocks_from_logo`].
    ///
    /// Not simply a smaller `min_absent`: a programme's own full-screen
    /// caption card takes the corner as thoroughly as an ident does, and
    /// what tells them apart is the sound -- which runs through a caption
    /// card and stops for an insert. A programme that lays its cards over
    /// silence defeats that, and one measured does.
    ///
    /// **1.75 is a gap in the measurement, not a round number.** Sixty-six
    /// short absences over twenty episodes of that programme come out as two
    /// groups with nothing between them: twenty-five between 1.53 and 1.67
    /// seconds, every one of them a caption card, and forty-one from 1.83
    /// upwards, which is where the idents are.
    pub min_insert: f64,
    /// ...except at the two ends. An absence that runs off the start or the
    /// end of the recording is not a break at all -- it is the recorder
    /// having begun before the programme did, or stopped after it ended --
    /// and those are routinely only a few seconds long.
    pub min_edge_absent: f64,
    /// How long the logo must stay up before an absence counts as over.
    /// Shorter than the window the scores were smoothed over, a return
    /// cannot register at full height anyway, so it is not evidence of one.
    pub min_present: f64,
    /// A run of commercials is long. If the absences found are mostly short,
    /// what is being tracked is not a logo.
    pub typical_break: f64,
    /// More absences than a broadcast could plausibly contain means the
    /// signal is noise.
    pub max_breaks: usize,
    /// Seconds of samples averaged before scoring, to wash out the picture
    /// behind the logo. Too short and the content still shows through.
    pub window_seconds: f64,
    /// How many pixels the template keeps. A station logo is small, so a
    /// fixed count focuses on it however large the search region is -- a
    /// percentage of a big region is mostly the noise around it.
    pub mask_pixels: usize,
    /// How many cores the decode may have. Zero, the default, is all of them.
    pub threads: usize,
}

impl Default for LogoOptions {
    fn default() -> Self {
        Self {
            present: 0.05,
            absent: 0.02,
            min_absent: 20.0,
            min_insert: 1.75,
            min_edge_absent: 1.0,
            min_present: 5.0,
            typical_break: 30.0,
            max_breaks: 12,
            window_seconds: 5.0,
            mask_pixels: 500,
            threads: 0,
        }
    }
}

/// One corner of the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    const ALL: [Corner; 4] = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomLeft,
        Corner::BottomRight,
    ];

    fn origin(self, w: usize, h: usize, cw: usize, ch: usize) -> (usize, usize) {
        match self {
            Corner::TopLeft => (0, 0),
            Corner::TopRight => (w - cw, 0),
            Corner::BottomLeft => (0, h - ch),
            Corner::BottomRight => (w - cw, h - ch),
        }
    }
}

pub struct Logo {
    pub corner: Corner,
    /// How strongly the template stands out.
    pub strength: f64,
    /// Stretches, in seconds, where the logo is not on screen.
    pub absent: Vec<(f64, f64)>,
    /// The stretches that fell short of [`LogoOptions::min_absent`] but
    /// reach [`LogoOptions::min_insert`].
    ///
    /// A few seconds of corner with no logo in it is either a channel ident
    /// dropped into the programme or the programme's own full-screen caption
    /// card, and **nothing in the corner tells them apart**. A card carries
    /// the logo -- drawn grey on black rather than over the picture -- and
    /// the correlation still falls to nought on it, because the template was
    /// built from the logo as it is drawn over a picture. Measured on two
    /// recordings, the lowest score reached inside an ident and inside a card
    /// are the same number. The caller decides, and on something else.
    pub brief: Vec<(f64, f64)>,
}

/// Nothing in any corner behaved like a station logo.
///
/// Not every broadcaster shows one -- several subscription channels do not --
/// and a detector that returns its best guess regardless would hand back
/// noise. Saying so lets the caller fall back to the silences alone.
#[derive(Debug)]
pub struct NoLogo;

impl std::fmt::Display for NoLogo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no station logo found")
    }
}

impl std::error::Error for NoLogo {}

struct Region {
    w: usize,
    h: usize,
}

impl Region {
    fn highpass(&self, v: &[f64]) -> Vec<f64> {
        let (w, h) = (self.w, self.h);
        let mut out = vec![0.0; v.len()];
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let i = y * w + x;
                out[i] = v[i] - (v[i - 1] + v[i + 1] + v[i - w] + v[i + w]) / 4.0;
            }
        }
        out
    }
}

/// Pull one corner's luma out of a decoded frame.
fn crop(frame: &ff::frame::Video, corner: Corner, cw: usize, ch: usize, out: &mut [u8]) {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let (ox, oy) = corner.origin(w, h, cw, ch);
    let stride = frame.stride(0);
    let data = frame.data(0);
    for y in 0..ch {
        let src = (oy + y) * stride + ox;
        out[y * cw..(y + 1) * cw].copy_from_slice(&data[src..src + cw]);
    }
}

/// All four corners of one picture, at full size. What the first pass
/// averages.
fn corners_of(frame: &ff::frame::Video, cw: usize, ch: usize) -> Option<Vec<Vec<u8>>> {
    if (frame.width() as usize) < cw || (frame.height() as usize) < ch {
        return None;
    }
    Some(
        Corner::ALL
            .iter()
            .map(|corner| {
                let mut buf = vec![0u8; cw * ch];
                crop(frame, *corner, cw, ch, &mut buf);
                buf
            })
            .collect(),
    )
}

/// The handful of samples a corner's score is actually worked out from.
///
/// The correlation is taken over the mask alone -- a few hundred pixels of a
/// region of a hundred thousand -- and the high pass in front of it reaches
/// one pixel each way. So the whole of what the second pass needs of a corner
/// is the mask and the ring around it, which is a fortieth of the corner, and
/// carrying the rest home was the greater part of what that pass cost.
fn wanted_of(
    frame: &ff::frame::Video,
    cw: usize,
    ch: usize,
    wanted: &[Vec<usize>],
) -> Option<Vec<Vec<u8>>> {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    if w < cw || h < ch {
        return None;
    }
    let stride = frame.stride(0);
    let data = frame.data(0);
    Some(
        Corner::ALL
            .iter()
            .enumerate()
            .map(|(k, corner)| {
                let (ox, oy) = corner.origin(w, h, cw, ch);
                wanted[k]
                    .iter()
                    .map(|&i| data[(oy + i / cw) * stride + ox + i % cw])
                    .collect()
            })
            .collect(),
    )
}

/// Decode the entry pictures and hand what `take` makes of each to `visit`.
///
/// **On as many cores as the machine will give it.** One decoder handed the
/// entry pictures of a recording is one core's worth of work whatever else is
/// idle, and this pass makes two of those walks over the whole file. The
/// pictures are spread over a pool instead, exactly as the thumbnail pass and
/// the film strip spread theirs; `take` runs on the worker that decoded the
/// picture, so pulling the corners out of a frame is spread with it and what
/// crosses back between threads is the small thing.
///
/// **And the entry pictures are picked out by packet, not by `skip_frame`.**
/// That switch is per picture, and a field-coded entry point in broadcast
/// MPEG-2 is an I top field followed by a P bottom field in one packet:
/// `AVDISCARD_NONKEY` throws the bottom field away and leaves half a picture
/// or none at all. [`crate::EntryPictures`] reads the packets instead, which
/// is what every other pass here does with them.
fn walk_keyframes<T: Send + 'static>(
    src: &Source,
    reach: &Reach,
    threads: usize,
    take: impl Fn(&ff::frame::Video) -> Option<T> + Send + Sync + 'static,
    mut progress: Option<&mut dyn FnMut(f64)>,
    mut visit: impl FnMut(f64, T),
) -> Result<()> {
    let mut ictx = crate::input::demux(&src.input.url)?;
    let idx = src.video.stream_index;
    // Only the pictures are read. See [`crate::input::keep_only`].
    crate::input::keep_only(&mut ictx, &[idx]);
    let params = ictx
        .stream(idx)
        .ok_or_else(|| anyhow!("video stream vanished"))?
        .parameters();
    let mut pool = crate::entrypool::Pool::new(
        params,
        &src.video,
        src.start_time,
        crate::entrypool::width(threads, &src.video),
        // Behind the rest of the machine: nobody is watching this pass
        // picture by picture. See [`crate::nice`].
        crate::entrypool::Standing::Behind,
        move |t, frame| Ok(take(frame).map(|v| (t, v))),
    )?;

    let mut deliver = |made: Vec<Option<(f64, T)>>| {
        let mut last = None;
        for (t, v) in made.into_iter().flatten() {
            visit(t, v);
            last = Some(t);
        }
        last
    };

    let (starts, each) = match reach {
        Reach::Whole => (vec![None], usize::MAX),
        Reach::Stretches { from, each } => {
            (from.iter().map(|&t| Some(t)).collect(), (*each).max(1))
        }
    };
    let duration = src.duration.max(1e-9);
    let mut pending: Vec<ff::Packet> = Vec::new();
    for (n, start) in starts.iter().enumerate() {
        if let Some(at) = *start {
            // Seek with the streams back, then throw them away again: the
            // recording is seeked by one of them. See
            // [`crate::input::keep_only`].
            crate::input::keep_everything(&mut ictx);
            let placed = crate::index::seek_to_entry(&mut ictx, src, at).is_some();
            crate::input::keep_only(&mut ictx, &[idx]);
            if !placed {
                // The index could not place it, and reading on from wherever
                // the file stands would sample the same stretch twice.
                continue;
            }
        }
        // A seek leaves whatever was half sent behind it.
        pending.clear();
        let mut entries = crate::EntryPictures::new(&src.video);
        let mut took = 0usize;
        // One picture's packets: the key packet, and the one behind it where
        // the material is field coded and the picture is two.
        for (stream, packet) in ictx.packets() {
            if stream.index() != idx {
                continue;
            }
            match entries.step(&packet) {
                crate::Step::Skip => continue,
                crate::Step::Half => pending.push(packet),
                crate::Step::Whole => {
                    pending.push(packet);
                    pool.push(std::mem::take(&mut pending));
                    took += 1;
                    let mut last = None;
                    while let Some(made) = pool.ready() {
                        last = deliver(made?).or(last);
                    }
                    if let Some(f) = progress.as_mut() {
                        f(match (last, each) {
                            (_, e) if e != usize::MAX => {
                                (n as f64 + took as f64 / e as f64) / starts.len() as f64
                            }
                            (Some(t), _) => (t / duration).clamp(0.0, 1.0),
                            (None, _) => 0.0,
                        });
                    }
                    if took >= each {
                        break;
                    }
                }
            }
        }
    }
    for made in pool.drain() {
        deliver(made?);
    }
    Ok(())
}

/// Which entry pictures a walk is to decode.
enum Reach {
    /// Every one of them, reading the recording from end to end.
    Whole,
    /// `each` of them from every one of these instants.
    Stretches { from: Vec<f64>, each: usize },
}

/// How many entry pictures the template is built from, where the recording
/// holds enough more than that to be worth not reading.
///
/// The template is an average, and an average of a couple of thousand
/// broadcast pictures has the picture behind the logo blurred away already:
/// it is the *number* of pictures that does that and not the share of the
/// recording they came from. A 30-minute recording carries 3600 entry
/// pictures and two and a half hours carries seventeen thousand, and the
/// second one does not need a template five times as good.
const TEMPLATE_PICTURES: usize = 1800;

/// ...taken in this many runs spread across the recording, rather than one
/// picture in every N.
///
/// What is being saved is the reading, and on a share that is the whole of
/// what this pass costs. A run of packets is read at the share's speed and a
/// scattered sample at its seek time, so the sample is contiguous where it
/// can be and spread where it matters: a template drawn from one stretch of
/// a recording has whatever was on screen during that stretch standing as
/// still as the logo.
const TEMPLATE_STRETCHES: usize = 24;

/// Where the template's pass is to read, given what the recording holds.
///
/// Reading end to end wherever the saving would be small: a recording with
/// twice the sample in it saves half a read, and one with barely more than
/// the sample saves nothing and pays for the seeks.
fn template_reach(src: &Source) -> Reach {
    let points = src.points.len();
    // ...and only where every entry point says what byte it starts at, which
    // is what the seek is made of. An index that does not carry them would
    // place no run at all and the pass would come back with nothing to build
    // a template from, which reads from outside as a recording with no logo.
    // The switch that turns byte seeks off for a measurement turns them off
    // here too: `seek_to_entry` would refuse every run and leave nothing.
    if !src.byte_seekable
        || crate::index::off("SMARTCUT_BYTE_SEEK")
        || points < TEMPLATE_PICTURES * 2
        || src.duration <= 0.0
        || src.points.iter().any(|p| p.pos < 0)
    {
        return Reach::Whole;
    }
    let each = TEMPLATE_PICTURES.div_ceil(TEMPLATE_STRETCHES);
    let from = (0..TEMPLATE_STRETCHES)
        .map(|k| src.duration * k as f64 / TEMPLATE_STRETCHES as f64)
        .collect();
    Reach::Stretches { from, each }
}

pub fn detect(src: &Source, opts: &LogoOptions) -> Result<Logo> {
    detect_with(src, opts, None)
}

/// As [`detect`], reporting progress across both passes over the key
/// pictures. The first builds the template, the second scores against it, so
/// each is half the work.
pub fn detect_with(
    src: &Source,
    opts: &LogoOptions,
    mut progress: Option<Box<dyn FnMut(f64) + Send>>,
) -> Result<Logo> {
    crate::init()?;
    let (w, h) = (src.video.width as usize, src.video.height as usize);
    let (cw, ch) = ((w / 4) & !1, (h / 5) & !1);
    if cw < 16 || ch < 16 {
        return Err(anyhow!("frame too small to look for a logo"));
    }
    let region = Region { w: cw, h: ch };
    let n = cw * ch;

    // First pass: what is always there, in each corner?
    let mut sums: Vec<Vec<f64>> = (0..4).map(|_| vec![0.0; n]).collect();
    let mut count = 0usize;
    // How far apart the entry pictures stand, which is what the second
    // pass's smoothing window is measured in. Taken from the pictures
    // themselves rather than from the count over the length: the template's
    // pass may have sampled the recording in runs, and a count over the
    // whole length would then read as a quarter of the real rate.
    let mut gaps: Vec<f64> = Vec::new();
    let mut previous = f64::NEG_INFINITY;
    // Two passes over the key pictures, so each is half of the reported
    // progress. The adaptor is scoped to its pass: it borrows `progress`,
    // and the second pass needs it back.
    {
        let mut told = -1.0;
        let mut on = |done: f64| {
            if let Some(f) = progress.as_mut() {
                if done - told >= 0.02 {
                    told = done;
                    f(done * 0.5);
                }
            }
        };
        walk_keyframes(
            src,
            &template_reach(src),
            opts.threads,
            move |frame| corners_of(frame, cw, ch),
            Some(&mut on),
            |t, corners| {
                for (k, buf) in corners.iter().enumerate() {
                    for (i, &v) in buf.iter().enumerate() {
                        sums[k][i] += v as f64;
                    }
                }
                if previous.is_finite() && t > previous {
                    gaps.push(t - previous);
                }
                previous = t;
                count += 1;
            },
        )?;
    }
    if count < 20 {
        return Err(anyhow!(
            "only {count} key frames; not enough to find a logo"
        ));
    }

    // Build a template for each corner. The strongest is not necessarily the
    // logo: programme branding is often bolder, but it comes and goes.
    struct Cand {
        corner: Corner,
        tmpl: Vec<f64>,
        mask: Vec<usize>,
        norm: f64,
        strength: f64,
    }
    let mut cands = Vec::new();
    for (k, corner) in Corner::ALL.iter().enumerate() {
        let avg: Vec<f64> = sums[k].iter().map(|s| s / count as f64).collect();
        let tmpl = region.highpass(&avg);
        // The picture's own edge stands as still as any logo does, and it is
        // the strongest thing in the region: on a recording with pillar-box
        // bars the mask came out as a 428-pixel line two pixels wide running
        // the height of the corner, which is not a mark a station puts on a
        // broadcast. Since it never goes away it reads as a logo that is
        // never absent, and the recording gets no breaks for the wrong
        // reason.
        let inside = |i: usize| {
            let (x, y) = (i % cw, i / cw);
            x >= MARGIN && y >= MARGIN && x < cw - MARGIN && y < ch - MARGIN
        };
        let mut mag: Vec<f64> = (0..n)
            .filter(|&i| inside(i))
            .map(|i| tmpl[i].abs())
            .collect();
        mag.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        let cutoff = mag[opts.mask_pixels.min(mag.len().saturating_sub(1))];
        let picked: Vec<usize> = (0..n)
            .filter(|&i| inside(i) && tmpl[i].abs() >= cutoff)
            .collect();
        // A logo is one mark. Whatever else happened to sit still -- the edge
        // of a caption box, a graphic a commercial holds on screen -- is
        // somewhere else in the corner, and only adds noise to the
        // correlation.
        let mask = largest_cluster(&picked, cw, ch);
        let strength =
            mask.iter().map(|&i| tmpl[i] * tmpl[i]).sum::<f64>() / mask.len().max(1) as f64;
        let norm = mask.iter().map(|&i| tmpl[i] * tmpl[i]).sum::<f64>().sqrt();
        cands.push(Cand {
            corner: *corner,
            tmpl,
            mask,
            norm,
            strength,
        });
    }

    // Second pass: score every key frame against all four templates.
    let ring_len = {
        // Key pictures are not evenly spaced, so size the window from what
        // they actually turned out to be. The median stands up to a sampled
        // pass: the jumps between one run and the next are two dozen gaps
        // out of eighteen hundred.
        gaps.sort_by(f64::total_cmp);
        let gap = gaps.get(gaps.len() / 2).copied().unwrap_or(0.5).max(1e-6);
        ((opts.window_seconds / gap).round() as usize).clamp(2, 64)
    };
    // Which samples of each corner the score is worked out from: the mask,
    // and the four neighbours the high pass reaches for at each of them. Every
    // mask pixel is at least [`MARGIN`] inside the region, so all four exist.
    // `slots` is the way back -- where a region index sits in that list -- and
    // is a lookup rather than a search because it is read once per mask pixel
    // per picture.
    let wanted: Vec<Vec<usize>> = cands
        .iter()
        .map(|c| {
            let mut v: Vec<usize> = c
                .mask
                .iter()
                .flat_map(|&i| [i, i - 1, i + 1, i - cw, i + cw])
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        })
        .collect();
    let slots: Vec<Vec<u32>> = wanted
        .iter()
        .map(|w| {
            let mut back = vec![0u32; n];
            for (j, &i) in w.iter().enumerate() {
                back[i] = j as u32;
            }
            back
        })
        .collect();
    let mut sums: Vec<Vec<u32>> = wanted.iter().map(|w| vec![0u32; w.len()]).collect();
    let mut rings: Vec<std::collections::VecDeque<Vec<u8>>> =
        (0..4).map(|_| Default::default()).collect();
    let mut times: Vec<f64> = Vec::new();
    let mut scores: Vec<Vec<f64>> = vec![Vec::new(); 4];
    {
        let mut told = -1.0;
        let mut on = |done: f64| {
            if let Some(f) = progress.as_mut() {
                if done - told >= 0.02 {
                    told = done;
                    f(0.5 + done * 0.5);
                }
            }
        };
        walk_keyframes(
            src,
            &Reach::Whole,
            opts.threads,
            move |frame| wanted_of(frame, cw, ch, &wanted),
            Some(&mut on),
            |t, corners| {
                times.push(t);
                for k in 0..4 {
                    // The window average, kept rather than worked out afresh:
                    // a picture joins the ring and the picture that falls out
                    // of the other end leaves it, which is two touches of each
                    // sample where summing the whole ring was `ring_len` of
                    // them.
                    for (j, &v) in corners[k].iter().enumerate() {
                        sums[k][j] += v as u32;
                    }
                    rings[k].push_back(corners[k].clone());
                    if rings[k].len() > ring_len {
                        let gone = rings[k].pop_front().expect("just checked");
                        for (j, &v) in gone.iter().enumerate() {
                            sums[k][j] -= v as u32;
                        }
                    }
                    let held = rings[k].len() as f64;
                    let c = &cands[k];
                    let avg = |i: usize| sums[k][slots[k][i] as usize] as f64 / held;
                    let hp = |i: usize| {
                        avg(i) - (avg(i - 1) + avg(i + 1) + avg(i - cw) + avg(i + cw)) / 4.0
                    };
                    let mut dot = 0.0;
                    let mut en = 0.0;
                    for &i in &c.mask {
                        let v = hp(i);
                        dot += v * c.tmpl[i];
                        en += v * v;
                    }
                    let en = en.sqrt();
                    scores[k].push(if en > 0.0 && c.norm > 0.0 {
                        dot / (en * c.norm)
                    } else {
                        0.0
                    });
                }
            },
        )?;
    }

    // Thresholds from the recording itself: how boldly a logo reads varies
    // by channel, but the programme always dominates the runtime, so the
    // median score is a reliable stand-in for "logo present".
    let thresholds: Vec<(f64, f64)> = scores
        .iter()
        .map(|col| {
            let mut v = col.clone();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let median = v.get(v.len() / 2).copied().unwrap_or(0.0);
            ((median * 0.40).max(0.01), (median * 0.15).max(0.004))
        })
        .collect();

    // Pick the corner that behaves like a station logo: on for most of the
    // recording, and switching state only a handful of times. Programme
    // branding scores higher but flickers; the logo is steady.
    /// The corner that won, how unsettled its signal was, and what it found.
    struct Pick {
        corner_index: usize,
        transitions: usize,
        /// Share of the recording the corner read as carrying its mark.
        /// Only used to settle a tie: see where `chosen` is decided.
        present: f64,
        absent: Vec<(f64, f64)>,
        brief: Vec<(f64, f64)>,
    }
    let mut chosen: Option<Pick> = None;
    for k in 0..4 {
        let (present_t, absent_t) = thresholds[k];
        let (intervals, brief, transitions) =
            intervals_from(&times, &scores[k], present_t, absent_t, opts);
        let present: usize = scores[k].iter().filter(|&&s| s >= present_t).count();
        let frac = present as f64 / scores[k].len().max(1) as f64;
        if std::env::var("SMARTCUT_DEBUG").is_ok() {
            eprintln!(
                "  corner {:?}: strength {:8.1}  present {:.3}  transitions {}  intervals {}",
                cands[k].corner,
                cands[k].strength,
                frac,
                transitions,
                intervals.len()
            );
        }
        // A logo is on screen for most of a broadcast, and the gaps are
        // commercial breaks: few of them, and long. Short flickering gaps
        // mean the template latched onto moving picture instead.
        let mut lengths: Vec<f64> = intervals.iter().map(|(a, b)| b - a).collect();
        lengths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median_len = lengths
            .get(lengths.len() / 2)
            .copied()
            .unwrap_or(f64::INFINITY);
        let plausible = frac >= 0.5
            && intervals.len() <= opts.max_breaks
            && (intervals.is_empty() || median_len >= opts.typical_break);
        if !plausible {
            continue;
        }
        // Fewest state changes wins, and where two corners are equally
        // steady, the one that is on for more of the recording. The tie is
        // not hypothetical: a terrestrial recording whose station logo sits
        // top right also carries programme branding bottom left, and once
        // both templates were whole marks rather than fragments the branding
        // flipped state exactly as often as the logo did -- eight times each.
        // Iteration order then decided it, and the branding, which is up for
        // 56% of the recording against the logo's 76%, won and put a
        // four-minute "absence" over the programme.
        let better = chosen.as_ref().is_none_or(|p| {
            transitions < p.transitions || (transitions == p.transitions && frac > p.present)
        });
        if better {
            chosen = Some(Pick {
                corner_index: k,
                transitions,
                present: frac,
                absent: intervals,
                brief,
            });
        }
    }
    let pick = chosen.ok_or(NoLogo)?;
    let (absent, brief, k) = (pick.absent, pick.brief, pick.corner_index);
    let corner = cands[k].corner;
    let strength = cands[k].strength;

    Ok(Logo {
        corner,
        strength,
        absent,
        brief,
    })
}

/// Turn a score timeline into "logo missing" stretches, with hysteresis so a
/// single dark frame does not end the programme. Also reports how often the
/// state flipped, which is what tells a steady logo from a flickering caption.
///
/// The hysteresis is asymmetric in time as well as in level. Going absent is
/// believed at once, but a return has to be held for [`LogoOptions::min_present`]
/// before it ends the absence: inside a break the correlation occasionally
/// grazes the threshold for a frame or two on a commercial that happens to
/// resemble the template, and splitting one break there produces blocks that
/// overlap -- each edge is snapped to its own nearest junction, and the
/// earlier block's end can pass the later block's start.
#[allow(clippy::type_complexity)]
fn intervals_from(
    times: &[f64],
    scores: &[f64],
    present_t: f64,
    absent_t: f64,
    opts: &LogoOptions,
) -> (Vec<(f64, f64)>, Vec<(f64, f64)>, usize) {
    // Each stretch, and whether it runs off an end of the recording -- an
    // edge is held to a much shorter minimum than a break is.
    let mut absent: Vec<(f64, f64, bool)> = Vec::new();
    let mut transitions = 0usize;
    let mut present = scores.first().copied().unwrap_or(0.0) >= present_t;
    let mut at_head = !present;
    let mut from = times.first().copied().unwrap_or(0.0);
    // While absent: when the logo first came back, if it has been back ever
    // since. Going absent needs one sample; coming back needs to be held.
    let mut back: Option<f64> = None;
    // ...and the mirror of it while present: when the score first fell off,
    // if it has been down ever since.
    //
    // **An absence begins where the logo went, not where it was confirmed
    // gone.** The score is an average over the seconds behind it, so it
    // reaches the lower threshold only once most of that window is inside
    // the break -- measured on a BS recording with a pale logo, seven
    // seconds after the commercials started, which put the block's start
    // inside the first spot. The end of an absence was already read this
    // way; the start was not.
    let mut going: Option<f64> = None;
    for (i, &s) in scores.iter().enumerate() {
        if present {
            if s < present_t {
                going.get_or_insert(times[i]);
            } else {
                going = None;
            }
            if s < absent_t {
                present = false;
                transitions += 1;
                // Back no further than the window that was averaged. What is
                // being undone is that average's lag, and a score that has
                // been sliding for half a minute -- a dark scene at the end
                // of a programme -- is not a logo that went half a minute
                // ago. Past the window the correction would be swallowing
                // programme, which is the expensive mistake.
                from = going
                    .take()
                    .map_or(times[i], |t| t.max(times[i] - opts.window_seconds));
                back = None;
            }
        } else if s >= present_t {
            // An absence ends where the logo returned, not where the return
            // was confirmed -- the wait is only to establish that it was one.
            let since = *back.get_or_insert(times[i]);
            if times[i] - since >= opts.min_present {
                present = true;
                transitions += 1;
                absent.push((from, since, at_head));
                at_head = false;
                back = None;
            }
        } else {
            back = None;
        }
    }
    if !present {
        if let Some(&last) = times.last() {
            absent.push((from, last, true));
        }
    }

    let long = |a: f64, b: f64, edge: bool| {
        b - a
            >= if edge {
                opts.min_edge_absent
            } else {
                opts.min_absent
            }
    };
    // The short ones are handed back beside the long ones rather than
    // thrown away: a few seconds of corner with no logo in it is either a
    // channel ident dropped into the programme or the programme's own
    // caption card. See [`LogoOptions::min_insert`].
    let brief = absent
        .iter()
        .filter(|&&(a, b, edge)| !long(a, b, edge) && b - a >= opts.min_insert)
        .map(|&(a, b, _)| (a, b))
        .collect();
    let absent = absent
        .into_iter()
        .filter(|&(a, b, edge)| long(a, b, edge))
        .map(|(a, b, _)| (a, b))
        .collect();
    (absent, brief, transitions)
}

/// How far apart two mask pixels may sit and still belong to the same mark.
///
/// Touching is too strict a test for a logo made of thin strokes. A pale
/// watermark high-passes to a scatter of one- and two-pixel fragments: on the
/// recording this was measured against, the top 500 pixels of the corner fell
/// into 290 clusters, the largest of them fifteen pixels of a single stroke,
/// and a correlation over fifteen pixels is noise -- which is why that
/// recording's logo was never tracked and the corner was dismissed as not
/// carrying one.
///
/// Three pixels of reach joins the strokes of one mark without joining marks
/// that sit apart. The same corner then gives one cluster of 156 pixels
/// covering the whole logo, while the commercial's own graphics, ten pixels
/// below it, stay a cluster of their own. Five pixels of reach would still
/// have kept them apart on this recording; three leaves room for a station
/// that sets its logo closer to something else.
const REACH: i64 = 3;

/// How much of the region's own border the mask may not be drawn from.
///
/// Four pixels is enough to clear the step between a pillar-box bar and the
/// picture, which is what the mask latched onto otherwise. No station sets
/// its logo that tight against the edge of the frame.
const MARGIN: usize = 4;

/// Keep only the biggest connected run of mask pixels.
///
/// Pixels within [`REACH`] of each other count as connected, but only the
/// pixels that were actually picked are kept: the reach decides what belongs
/// to one mark, and is not itself part of the template.
fn largest_cluster(picked: &[usize], w: usize, h: usize) -> Vec<usize> {
    use std::collections::HashSet;
    let set: HashSet<usize> = picked.iter().copied().collect();
    let mut seen: HashSet<usize> = HashSet::new();
    let mut best: Vec<usize> = Vec::new();
    for &start in picked {
        if seen.contains(&start) {
            continue;
        }
        let mut group = Vec::new();
        let mut stack = vec![start];
        seen.insert(start);
        while let Some(i) = stack.pop() {
            group.push(i);
            let (x, y) = (i % w, i / w);
            for dy in -REACH..=REACH {
                for dx in -REACH..=REACH {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if set.contains(&j) && seen.insert(j) {
                        stack.push(j);
                    }
                }
            }
        }
        if group.len() > best.len() {
            best = group;
        }
    }
    best
}
