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
    let planes = if frame.is_planar() { frame.planes() } else { 1 };
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
        (1.0..=8.0).contains(&units) && (gap - units * 15.0).abs() <= RESET_GRID
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

/// What each reading is worth when the whole recording is divided at once.
///
/// [`plan`] weighs these against each other, so they share one unit: a
/// second of programme that the logo vouches for. A boundary's cost then
/// reads as "how many seconds of evidence a division here has to be worth
/// before it pays for itself".
pub struct PlanWeights {
    /// Per second the logo agrees with the label given to it.
    pub logo: f64,
    /// Per second, scaled by how full of junctions the run is...
    pub fill: f64,
    /// ...measured against this share of the fifteen-second grid filled. A
    /// run of commercials is silent at nearly every boundary; a talkative
    /// programme pauses on the beat about half the time.
    pub fill_floor: f64,
    /// A break has to be at least this much logo-less. Where the logo is up
    /// it is not evidence to be outweighed: the stretch is simply not a
    /// break, whatever the junctions look like.
    pub logo_says: f64,
    /// What a boundary costs at a silence, eased by how long the silence is.
    pub cost_silence: f64,
    /// ...at a caption reset. Negative, because a reset is not merely a
    /// permitted place to divide the recording: it is the broadcaster's own
    /// equipment saying that what it was captioning has stopped, stamped to
    /// the frame. A division that lands on one is better than a division
    /// that does not, and the logo -- a moving average whose edges lag by
    /// the window it was averaged over -- should not be able to drag an end
    /// past it.
    pub cost_reset: f64,
    /// ...where the logo went away or came back.
    pub cost_logo_edge: f64,
    /// No commercial break is shorter than this, or longer than that.
    pub min_break: f64,
    pub max_break: f64,
    /// A scrap of programme shorter than this between two breaks is not a
    /// return to the programme; it is a gap inside one break.
    pub min_programme: f64,
    /// How far inside its own edges a logo absence is believed. The score it
    /// is built from is a moving average, so the absence reaches this much
    /// further out than the break does at each end. Taking the edges at face
    /// value costs programme: on a terrestrial recording the absence ran
    /// 1.2 s past the last caption reset, and the break was extended to the
    /// end of the recording over a second and a half of programme.
    pub logo_lag: f64,
}

impl Default for PlanWeights {
    fn default() -> Self {
        Self {
            logo: 3.0,
            fill: 1.5,
            fill_floor: 0.5,
            logo_says: 0.5,
            cost_silence: 10.0,
            cost_reset: -10.0,
            cost_logo_edge: 20.0,
            min_break: 14.0,
            max_break: 400.0,
            min_programme: 20.0,
            logo_lag: 2.5,
        }
    }
}

/// How far outside a block a junction may sit and still be one of its own.
///
/// A junction is timed by the middle of a silence, which is only an estimate
/// of where the seam is -- and the block's own end may have been drawn at a
/// different reading of the same seam. On a terrestrial recording the last
/// caption reset and the middle of the silence around it are 0.17 s apart,
/// and counting to the tenth of a second lost the block a junction for
/// ending on the reset: enough of the grid to make running to the end of the
/// recording, over a second and a half of programme, score better.
const JUNCTION_SLACK: f64 = 0.75;

/// How many of a stretch's fifteen-second boundaries carry a junction, and
/// how many junctions that is.
///
/// Counted by boundary rather than by junction. A run of commercials is
/// silent at every fifteen-second mark because that is what the marks are,
/// so what says "commercials" is the grid being *complete* -- while a
/// talkative programme pauses several times within one unit and would
/// otherwise count as more than full. The block's own ends are marks too:
/// they are where it was cut.
fn grid_filled(junctions: &[f64], a: f64, b: f64) -> (usize, f64) {
    let slots = ((b - a) / 15.0).round().max(1.0) + 1.0;
    let mut seen = vec![false; slots as usize + 1];
    let mut count = 0usize;
    for &t in junctions {
        if t < a - JUNCTION_SLACK || t > b + JUNCTION_SLACK {
            continue;
        }
        count += 1;
        let slot = (((t - a) / 15.0).round().max(0.0) as usize).min(seen.len() - 1);
        seen[slot] = true;
    }
    let filled = seen.iter().filter(|&&x| x).count() as f64;
    (count, filled / slots)
}

/// Seconds of `a..b` covered by `spans`.
fn covered(a: f64, b: f64, spans: &[(f64, f64)]) -> f64 {
    spans
        .iter()
        .map(|&(x, y)| (b.min(y) - a.max(x)).max(0.0))
        .sum()
}

/// Divide the whole recording into programme and commercial in one decision.
///
/// The other entry points here decide locally and in a fixed order of
/// preference: resets if there are any, otherwise the logo, otherwise the
/// silences, each with its own thresholds. That order throws away whichever
/// readings come second, and a threshold cannot be argued with by evidence
/// that sits either side of it.
///
/// This asks the question once. Every reading offers boundaries -- a silence
/// centre, a reset, an edge of a logo absence -- and every division of the
/// recording into alternating stretches is scored by how well it explains
/// all of them at once. The best division is found by a dynamic program over
/// the boundaries, which is exact rather than greedy.
///
/// `logo_absent` is `None` when no logo was found, and `Some(&[])` for a logo
/// that was found and never went away -- which is not the absence of a
/// reading but the strongest statement in the recording that all of it is
/// programme.
pub fn plan(
    silences: &[Silence],
    resets: &[f64],
    logo_absent: Option<&[(f64, f64)]>,
    duration: f64,
    opts: &DetectOptions,
    w: &PlanWeights,
) -> Vec<Block> {
    let spans: Vec<(f64, f64)> = silences.iter().map(|s| (s.start, s.end)).collect();

    // A silence counts as a junction where another one sits a whole number
    // of commercial units away -- the same test the chain walk makes, kept
    // at the level of the boundaries rather than the length of a block.
    let junctions: Vec<f64> = silences
        .iter()
        .enumerate()
        .filter(|&(i, _)| {
            spans.iter().enumerate().any(|(j, _)| {
                j != i
                    && if j < i {
                        on_grid(spans[j], spans[i], opts)
                    } else {
                        on_grid(spans[i], spans[j], opts)
                    }
            })
        })
        .map(|(_, s)| s.centre())
        .collect();

    // Where a boundary may be drawn, and what it costs there. Where two
    // readings mark the same instant, the cheaper of them stands.
    let mut marks: Vec<(f64, f64)> = vec![(0.0, 0.0), (duration, 0.0)];
    for s in silences {
        let eased = 1.0 - 0.5 * (s.duration().min(1.5) / 1.5);
        marks.push((s.centre(), w.cost_silence * eased));
    }
    marks.extend(resets.iter().map(|&t| (t, w.cost_reset)));
    if let Some(absent) = logo_absent {
        for &(a, b) in absent {
            marks.push((a, w.cost_logo_edge));
            marks.push((b, w.cost_logo_edge));
        }
    }
    marks.retain(|&(t, _)| (0.0..=duration).contains(&t));
    marks.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut ts: Vec<f64> = Vec::with_capacity(marks.len());
    let mut cost: Vec<f64> = Vec::with_capacity(marks.len());
    for (t, c) in marks {
        match ts.last() {
            Some(&last) if (t - last).abs() < 1e-9 => {
                let at = cost.len() - 1;
                cost[at] = cost[at].min(c);
            }
            _ => {
                ts.push(t);
                cost.push(c);
            }
        }
    }
    let n = ts.len();
    if n < 2 {
        return Vec::new();
    }

    // What the logo says, held to where it is sure of itself. Its edges are
    // still offered as boundaries -- that is where the break is, to within
    // the window -- but they do not count as evidence about which side of
    // the break those seconds fall on.
    let absent: Vec<(f64, f64)> = logo_absent
        .unwrap_or(&[])
        .iter()
        .filter_map(|&(a, b)| {
            // Only where there is a break to lag behind. An absence running
            // off either end of the recording is the recorder having started
            // or stopped inside one, and its outer end is the file's, not a
            // moving average's.
            let trim = ((b - a) / 4.0).min(w.logo_lag);
            let head = if a <= EDGE { a } else { a + trim };
            let tail = if b >= duration - EDGE { b } else { b - trim };
            (tail > head).then_some((head, tail))
        })
        .collect();
    let absent = absent.as_slice();
    let have_logo = logo_absent.is_some();
    let score_of = |a: f64, b: f64, is_break: bool| -> Option<f64> {
        let span = b - a;
        // The recording's own ends are not seams. What sits against them is
        // whatever the recorder caught of a break already running, and that
        // is routinely a few seconds rather than a whole unit.
        let at_edge = a <= EDGE || b >= duration - EDGE;
        let gone = covered(a, b, absent);
        if is_break {
            if span > w.max_break || (span < w.min_break && !at_edge) {
                return None;
            }
            if have_logo && gone < w.logo_says * span {
                return None;
            }
            let (_, fill) = grid_filled(&junctions, a, b);
            let mut s = w.fill * span * (fill - w.fill_floor);
            if have_logo {
                s += w.logo * (gone - (span - gone));
            }
            return Some(s);
        }
        if span < w.min_programme && !at_edge {
            return None;
        }
        Some(if have_logo {
            w.logo * ((span - gone) - gone)
        } else {
            0.0
        })
    };

    // best[i][k] is the best total for a division of 0..ts[i] whose last
    // stretch is a break when k is 1.
    let mut best = vec![[f64::NEG_INFINITY; 2]; n];
    let mut from = vec![[None::<(usize, usize)>; 2]; n];
    for i in 1..n {
        for k in 0..2 {
            for j in 0..i {
                let Some(s) = score_of(ts[j], ts[i], k == 1) else {
                    continue;
                };
                let prev = if j == 0 {
                    0.0
                } else {
                    let p = best[j][1 - k];
                    if p == f64::NEG_INFINITY {
                        continue;
                    }
                    p - cost[j]
                };
                if prev + s > best[i][k] {
                    best[i][k] = prev + s;
                    from[i][k] = Some((j, 1 - k));
                }
            }
        }
    }

    let mut k = usize::from(best[n - 1][1] > best[n - 1][0]);
    if best[n - 1][k] == f64::NEG_INFINITY {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut i = n - 1;
    while let Some((j, prev_k)) = from[i][k] {
        if k == 1 {
            let (a, b) = (ts[j], ts[i]);
            let (inside, fill) = grid_filled(&junctions, a, b);
            out.push(Block {
                start: a,
                end: b,
                junctions: inside,
                // How much of the block is actually evidenced, by whichever
                // reading has more to say about it.
                score: fill
                    .max(covered(a, b, absent) / (b - a).max(1e-9))
                    .clamp(0.0, 1.0),
            });
        }
        i = j;
        k = prev_k;
    }
    out.reverse();
    out
}
