//! Finding the places a commercial break is likely to start or end.
//!
//! Japanese broadcasters butt commercials together in 15-second units and
//! leave a short silence at every junction. Two things separate those
//! junctions from the pauses inside a programme:
//!
//! * they are longer -- around a second, against 0.1-0.4 s for a pause in
//!   dialogue;
//! * they line up on a 15-second grid with their neighbours.
//!
//! Neither is conclusive on its own, so what comes out of here is a ranked
//! list of *candidates*, not a decision.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;

use crate::Source;

/// A stretch of near-silence.
#[derive(Debug, Clone, Copy)]
pub struct Silence {
    pub start: f64,
    pub end: f64,
}

impl Silence {
    pub fn centre(&self) -> f64 {
        (self.start + self.end) / 2.0
    }

    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// A place worth offering as a cut point.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Middle of the silence -- where a cut would be least audible.
    pub time: f64,
    pub silence: f64,
    /// The silent stretch itself, which is where the junction may sit.
    pub start: f64,
    pub end: f64,
    /// How many junctions this one belongs to a 15-second run with. A long
    /// run is a commercial block; a lone silence is probably just a pause.
    pub run: usize,
    /// 0..1, from the run length and how long the silence is.
    pub score: f64,
}

pub struct DetectOptions {
    /// Everything quieter than this counts as silence.
    pub threshold_db: f64,
    /// Ignore anything shorter; programme pauses are brief.
    pub min_silence: f64,
    /// How far off a 15-second multiple a neighbour may sit, over and above
    /// what the silences themselves allow.
    pub grid_tolerance: f64,
    /// How full of junctions a block has to be to count as one. A run of
    /// commercials is silent at nearly every 15-second boundary, because that
    /// is what the boundaries are. A chain that only touches a few of them
    /// is dialogue that happened to pause on the beat -- and in a talkative
    /// programme the silences are already about one per fifteen seconds, so
    /// landing on the grid is no achievement at all.
    pub min_fill: f64,
    /// Ceiling on the slack the silences may contribute. A junction is timed
    /// by the middle of its silence, but the cut may be anywhere inside it,
    /// so a long silence is genuinely less certain about where the junction
    /// is -- up to a point, beyond which it is not a junction being measured
    /// but a quiet passage.
    pub grid_max_slack: f64,
}

impl Default for DetectOptions {
    fn default() -> Self {
        Self {
            threshold_db: -50.0,
            min_silence: 0.4,
            grid_tolerance: 0.15,
            grid_max_slack: 0.6,
            min_fill: 0.6,
        }
    }
}

/// Peak amplitude of one decoded audio frame, as a fraction of full scale.
fn frame_peak(frame: &ff::frame::Audio) -> f64 {
    use ff::format::sample::{Sample, Type};
    // A frame can claim more planes than an `AVFrame` has pointers to hold:
    // eight is all there are, and a corrupt header in an off-air recording is
    // enough to ask for a ninth. Reading it is a panic rather than an error,
    // and it took the whole detection down on one recording in a survey of
    // thirty-five. What the loudest channel is does not change for looking at
    // the first eight.
    let planes = if frame.is_planar() {
        frame.planes().min(8)
    } else {
        1
    };
    let mut peak = 0.0f64;
    for p in 0..planes {
        match frame.format() {
            Sample::F32(_) => {
                for &s in frame.plane::<f32>(p) {
                    peak = peak.max(s.abs() as f64);
                }
            }
            Sample::I16(_) => {
                for &s in frame.plane::<i16>(p) {
                    peak = peak.max(s.unsigned_abs() as f64 / 32768.0);
                }
            }
            Sample::I32(_) => {
                for &s in frame.plane::<i32>(p) {
                    peak = peak.max(s.unsigned_abs() as f64 / 2147483648.0);
                }
            }
            Sample::F64(_) => {
                for &s in frame.plane::<f64>(p) {
                    peak = peak.max(s.abs());
                }
            }
            _ => return 1.0, // unknown layout: treat as loud, never silent
        }
        let _ = Type::Packed;
    }
    peak
}

/// Walk the audio and note every stretch quieter than the threshold.
pub fn find_silences(src: &Source, opts: &DetectOptions) -> Result<Vec<Silence>> {
    find_silences_with(src, opts, None)
}

/// As [`find_silences`], reporting how far through the audio it has read.
pub fn find_silences_with(
    src: &Source,
    opts: &DetectOptions,
    mut progress: Option<Box<dyn FnMut(f64) + Send>>,
) -> Result<Vec<Silence>> {
    crate::init()?;
    let audio = src
        .audio
        .as_ref()
        .ok_or_else(|| anyhow!("{} has no audio", src.path))?;
    let mut ictx = crate::input::demux(&src.input.url)?;
    let params = ictx
        .stream(audio.stream_index)
        .ok_or_else(|| anyhow!("audio stream vanished"))?
        .parameters();
    let mut decoder = ff::codec::context::Context::from_parameters(params)?
        .decoder()
        .audio()?;

    let floor = 10f64.powf(opts.threshold_db / 20.0);
    let mut frame = ff::frame::Audio::empty();
    let mut out: Vec<Silence> = Vec::new();
    let mut quiet_from: Option<f64> = None;
    let mut last_end = 0.0;
    let mut told = -1.0;

    for (stream, packet) in ictx.packets() {
        if stream.index() != audio.stream_index {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        while decoder.receive_frame(&mut frame).is_ok() {
            let Some(pts) = frame.pts() else { continue };
            let t = pts as f64 * audio.time_base - src.start_time;
            let dur = frame.samples() as f64 / audio.sample_rate.max(1) as f64;
            if frame_peak(&frame) < floor {
                quiet_from.get_or_insert(t);
            } else if let Some(from) = quiet_from.take() {
                out.push(Silence {
                    start: from,
                    end: t,
                });
            }
            last_end = t + dur;
            if let Some(f) = progress.as_mut() {
                let done = (t / src.duration.max(1e-9)).clamp(0.0, 1.0);
                if done - told >= 0.02 {
                    told = done;
                    f(done);
                }
            }
        }
    }
    if let Some(f) = progress.as_mut() {
        f(1.0);
    }
    if let Some(from) = quiet_from {
        out.push(Silence {
            start: from,
            end: last_end,
        });
    }
    out.retain(|s| s.duration() >= opts.min_silence);
    Ok(out)
}

/// Rank the silences by how much they look like commercial junctions.
/// Could two junctions be a whole number of 15-second units apart?
///
/// Timing a junction by the middle of its silence is only an estimate: the
/// cut may be anywhere in the silent stretch, so a long silence says less
/// about where the junction is than a short one. Taking the slack from the
/// silences themselves is what lets one fixed rule fit channels whose
/// junction silences run 0.4s and channels whose run 1.4s -- with a ceiling,
/// because past a point a long quiet stretch is not a junction at all.
fn on_grid(a: (f64, f64), b: (f64, f64), opts: &DetectOptions) -> bool {
    let gap = ((b.0 + b.1) - (a.0 + a.1)) / 2.0;
    let slack = (((a.1 - a.0) + (b.1 - b.0)) / 2.0).min(opts.grid_max_slack) + opts.grid_tolerance;
    let units = (gap / 15.0).round();
    (1.0..=8.0).contains(&units) && (gap - units * 15.0).abs() <= slack
}

pub fn candidates(silences: &[Silence], opts: &DetectOptions) -> Vec<Candidate> {
    let spans: Vec<(f64, f64)> = silences.iter().map(|s| (s.start, s.end)).collect();

    let mut out = Vec::with_capacity(silences.len());
    for (i, s) in silences.iter().enumerate() {
        let mut run = 1usize;
        // Walk forwards then backwards along grid-aligned neighbours, taking
        // the *next* junction each way rather than the furthest one that
        // happens to sit on the grid.
        //
        // Both searches used to run up the list from nought, so the backward
        // one stepped straight to the earliest neighbour and skipped every
        // junction between. On a break of three junctions a minute apart,
        // the last one chained 60 s back to the first, found nothing before
        // that, and counted a run of two where its neighbours counted three:
        // it scored 0.51 against their 0.68, fell under the threshold, and
        // the break was left with two junctions and thrown away for it.
        for dir in [1i64, -1] {
            let mut at = i;
            loop {
                let next = if dir > 0 {
                    (at + 1..spans.len()).find(|&j| on_grid(spans[at], spans[j], opts))
                } else {
                    (0..at).rev().find(|&j| on_grid(spans[j], spans[at], opts))
                };
                match next {
                    Some(j) => {
                        run += 1;
                        at = j;
                    }
                    None => break,
                }
            }
        }
        let long = (s.duration() / 1.0).min(1.0);
        let chained = ((run.saturating_sub(1)) as f64 / 4.0).min(1.0);
        // Evenly weighted, which they were not while the run was being
        // undercounted. Counted properly, a chain runs long in any talkative
        // programme -- a pause every fifteen seconds is what conversation
        // sounds like -- so at 0.65 the chain alone all but decided it: a
        // 0.41 s pause, the shortest length this even looks at, chained four
        // deep and scored 0.63, over the 0.6 a block is built from, and
        // bridged the pauses either side of it into twenty-nine seconds of
        // "commercial" in a recording that has none. Half and half, that
        // pause scores 0.58 and the run it would have bridged never forms,
        // while a real junction -- a second of silence -- clears the line on
        // the strength of its own length.
        out.push(Candidate {
            time: s.centre(),
            silence: s.duration(),
            start: s.start,
            end: s.end,
            run,
            score: 0.5 * long + 0.5 * chained,
        });
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// A run of junctions: one commercial break, from its first cut point to its
/// last.
#[derive(Debug, Clone)]
pub struct Block {
    pub start: f64,
    pub end: f64,
    pub junctions: usize,
    pub score: f64,
}

impl Block {
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// Group junctions that sit on a common 15-second grid into breaks.
///
/// A single silence proves nothing, so only chains survive: the shape being
/// looked for is several junctions spaced 15, 30 or 60 seconds apart, which
/// is what a string of commercials produces and a conversation does not.
pub fn blocks(candidates: &[Candidate], opts: &DetectOptions, min_score: f64) -> Vec<Block> {
    let mut strong: Vec<&Candidate> = candidates.iter().filter(|c| c.score >= min_score).collect();
    strong.sort_by(|a, b| {
        a.time
            .partial_cmp(&b.time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut out: Vec<Block> = Vec::new();
    let mut current: Vec<&Candidate> = Vec::new();

    for c in strong {
        match current.last() {
            Some(prev) if on_grid((prev.start, prev.end), (c.start, c.end), opts) => {
                current.push(c)
            }
            Some(_) => {
                flush(&mut out, &current);
                current = vec![c];
            }
            None => current.push(c),
        }
    }
    flush(&mut out, &current);
    out.retain(|b| b.junctions >= 3 && fill(b) >= opts.min_fill);
    out
}

/// Put each block's boundaries on the picture the cut actually happens on.
///
/// A block arrives timed by estimates -- the middle of a silence, or the
/// moment a logo's rolling average crossed a threshold. Neither is a picture,
/// and on real material they land a fifth of a second to a third of a second
/// off. The cut itself is a change between consecutive pictures, and
/// [`crate::thumbs::cut_near`] finds which one.
pub fn refine_boundaries(src: &crate::Source, blocks: &mut [Block], window: f64, floor: f64) {
    // Nothing to refine against. A recording whose access points have not been
    // found yet -- see [`crate::Outline`] -- would send [`crate::thumbs::cut_near`]
    // looking for the entry point before each boundary, find none, and read the
    // file from its beginning to get there: twenty minutes of video decoded to
    // move one mark by a frame, and again for the next mark. The estimate the
    // block arrived with is a great deal better than that.
    if src.points.is_empty() {
        return;
    }
    for b in blocks.iter_mut() {
        // The head of the recording is not a cut; it is where the file starts.
        if b.start > 0.0 {
            b.start = crate::thumbs::cut_near(src, b.start, window, floor).unwrap_or(b.start);
        }
        if b.end < src.duration {
            b.end = crate::thumbs::cut_near(src, b.end, window, floor).unwrap_or(b.end);
        }
        b.end = b.end.max(b.start);
    }
}

/// How close to either end of the recording still counts as its edge.
const EDGE: f64 = 1.5;

/// What share of a block's 15-second boundaries actually carry a junction.
fn fill(b: &Block) -> f64 {
    let units = ((b.end - b.start) / 15.0).round().max(1.0);
    b.junctions as f64 / (units + 1.0)
}

fn flush(out: &mut Vec<Block>, run: &[&Candidate]) {
    if run.len() < 2 {
        return;
    }
    out.push(Block {
        start: run[0].time,
        end: run[run.len() - 1].time,
        junctions: run.len(),
        score: run.iter().map(|c| c.score).sum::<f64>() / run.len() as f64,
    });
}

/// Combine the two readings of where a break is.
///
/// They are good at different things. The silences give junction times that
/// are exact -- they sit on the 15-second grid the commercials were cut to --
/// but a run of them ends at the *last* junction, which is the start of the
/// final commercial, not its end. The logo knows where the break really ends,
/// but its own edges lag by however long a window was averaged to score it.
///
/// So: take the extent from the logo, and pull each edge onto the nearest
/// junction -- or onto the grid position continuing from the last one.
pub fn blocks_from_logo(
    candidates: &[Candidate],
    logo_absent: &[(f64, f64)],
    opts: &DetectOptions,
    snap: f64,
    duration: f64,
) -> Vec<Block> {
    let mut junctions: Vec<f64> = candidates
        .iter()
        .filter(|c| c.score >= 0.6)
        .map(|c| c.time)
        .collect();
    junctions.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let nearest = |t: f64, within: f64, from: &[f64]| -> Option<f64> {
        from.iter()
            .copied()
            .filter(|x| (x - t).abs() <= within)
            .min_by(|a, b| (a - t).abs().partial_cmp(&(b - t).abs()).unwrap())
    };

    logo_absent
        .iter()
        .map(|&(a, b)| {
            // the start lags by the scoring window, so allow a wider pull
            let start = nearest(a, snap * 2.0, &junctions).unwrap_or(a);

            // The break's own junctions, and the grid continuing past the
            // last of them: that is where the final commercial ends, and no
            // silence marks it because the programme simply resumes.
            let mine: Vec<f64> = junctions
                .iter()
                .copied()
                .filter(|&t| t >= start - 0.1 && t <= b + snap)
                .collect();
            let mut grid = mine.clone();
            if let Some(&last) = mine.last() {
                grid.extend((1..=8).map(|k| last + 15.0 * k as f64));
            }
            let end = nearest(b, snap, &grid).unwrap_or(b);
            let inside = mine.iter().filter(|&&t| t <= end + 0.1).count();
            // At the head there is nothing before the recording to keep, so
            // the block runs from the very start rather than from the first
            // picture the logo was missing on.
            let at_head = a <= EDGE;
            Block {
                start: if at_head { 0.0 } else { start },
                end: end.max(start),
                junctions: inside,
                score: if inside >= 2 { 1.0 } else { 0.7 },
            }
        })
        // An edge is what is left of a programme the recorder caught the end
        // or the beginning of, and those run to a few seconds where a break
        // never would.
        .filter(|blk| {
            let edge = blk.start <= EDGE || blk.end >= duration - EDGE;
            blk.duration() >= if edge { 1.0 } else { opts.min_silence.max(5.0) }
        })
        .collect()
}

/// How far off a 15-second multiple two resets may sit. Far tighter than the
/// silences are allowed, because a reset is a timestamp rather than a stretch
/// to guess a cut inside: measured across four recordings the worst neighbour
/// was 0.03 s off its grid position.
const RESET_GRID: f64 = 0.35;

/// A lone reset within one commercial unit of either end of the recording is
/// the recorder having started or stopped inside a break. Further in than
/// that, a single mark says a break happened here and nothing about where its
/// other end is, so nothing is offered.
const RESET_EDGE: f64 = 15.0;

/// How many 15-second units may lie between two resets and still be one break.
///
/// The silences are held to eight -- two minutes -- because a chain of them
/// forming by accident is the whole of what that reading has to defend
/// against. A reset is not an accident. It is a mark the broadcaster's own
/// equipment stamped on the junction, and the tolerance it is held to says
/// as much: 0.35 s against the silences' adaptive slack.
///
/// What eight cost instead was the stations that mark only the two ends of a
/// break rather than every junction inside it. There the whole break is one
/// gap, and a break is routinely longer than two minutes: measured across a
/// season of one BS channel's anime slots, the mid-programme break is 135 s
/// (nine units) or 150 s (ten) every time, and every one of them was thrown
/// away for being too long to be a break.
///
/// Twelve. Three minutes is longer than any mid-programme break measured and
/// far short of the stretch of programme that separates two of them, which
/// runs to twenty-five units and more. A station that marks every junction is
/// unaffected either way -- its resets are one unit apart, and a long break
/// there is a chain of ones.
const RESET_UNITS: f64 = 12.0;

/// How many of a recording's resets must sit one unit from the next before
/// the station counts as marking every junction rather than only the places
/// its programme stops and starts.
const RESET_INTERIOR: usize = 3;

/// Whether these marks are a reading of the recording's breaks, or only notes
/// on where some of them are.
///
/// Every station that marks anything marks the two ends of a break, so the
/// ends say nothing about which kind of station this is. What does is a mark
/// *inside* a break: two resets one 15-second unit apart, which is one
/// commercial ending and the next beginning. A station that marks every
/// junction leaves a run of those across each break; a station that marks
/// only where the programme stops and starts leaves none at all.
///
/// Measured over fifty half-hour recordings. The twenty-four from three
/// stations that mark throughout carry between 5 and 30 such adjacent pairs;
/// the twenty-six from a station that marks only the ends carry between 0 and
/// 2. [`RESET_INTERIOR`] sits in the middle of that gap.
///
/// It matters because the resets are read *instead of* the logo and the
/// silences rather than beside them. Where they are a complete reading that
/// is right, and the measurements behind the preference order say so: a mark
/// is exact where the other two guess. Where they are not, the preference
/// order reads six marks, emits one break, and never looks at the recording
/// that has four -- which is what the logo, left unread, had found.
pub fn marks_every_junction(resets: &[f64]) -> bool {
    resets
        .windows(2)
        .filter(|w| (w[1] - w[0] - 15.0).abs() <= RESET_GRID)
        .count()
        >= RESET_INTERIOR
}

/// Group caption resets into breaks.
///
/// The same shape as [`blocks`] -- junctions a whole number of 15-second
/// units apart -- but without the fill requirement, which exists to defend
/// against silences landing on the grid by accident. A reset is not an
/// accident: it is the caption service being told the programme it was
/// captioning has stopped. Broadcasters differ in how many junctions they
/// mark (one channel measured marks every one, another only some), so
/// demanding a full grid would throw away the sparser channel for no gain.
pub fn blocks_from_resets(resets: &[f64], duration: f64) -> Vec<Block> {
    let on_grid = |a: f64, b: f64| {
        let gap = b - a;
        let units = (gap / 15.0).round();
        (1.0..=RESET_UNITS).contains(&units) && (gap - units * 15.0).abs() <= RESET_GRID
    };

    let mut runs: Vec<Vec<f64>> = Vec::new();
    for &t in resets {
        match runs.last_mut() {
            Some(run) if on_grid(*run.last().expect("runs are never empty"), t) => run.push(t),
            _ => runs.push(vec![t]),
        }
    }

    runs.into_iter()
        .filter_map(|run| {
            let (first, last) = (run[0], *run.last().expect("runs are never empty"));
            if run.len() >= 2 {
                return Some(Block {
                    start: first,
                    end: last,
                    junctions: run.len(),
                    score: 1.0,
                });
            }
            // The recording's own ends are the only thing that can stand in
            // for the junction a lone mark is missing.
            if first <= RESET_EDGE {
                Some(Block {
                    start: 0.0,
                    end: first,
                    junctions: 1,
                    score: 0.9,
                })
            } else if duration - first <= RESET_EDGE {
                Some(Block {
                    start: first,
                    end: duration,
                    junctions: 1,
                    score: 0.9,
                })
            } else {
                None
            }
        })
        .collect()
}
