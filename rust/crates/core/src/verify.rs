//! Read a finished cut back and check it against what it was cut from.
//!
//! A smart-rendered cut makes two claims: the output holds exactly the
//! pictures that were asked for, and the ones that were not re-encoded are
//! the recording's own, bit for bit. The test suite has always checked both
//! (`smartcut/verify.py`), by decoding each side to one hash a picture and
//! lining the two lists up. This is the same check, made something a run can
//! ask for after it has written a file rather than something that only
//! happens on the fixtures.
//!
//! **Two decodes, side by side.** The source's kept ranges are decoded on one
//! thread and the output on another, and the pictures are compared as they
//! arrive -- nothing is held but a short queue between the two, so an
//! evening's recording is checked in the memory one picture takes. Each
//! pair is either the same picture exactly (a copied frame) or, where it is
//! not (a re-encoded one), close enough to be the same picture: the luma is
//! compared on a sparse grid and a picture under [`LIKE_DB`] is counted as a
//! different picture, which is what a frame out of place looks like.
//!
//! The output is also read for damage -- a picture the decoder had to
//! conceal -- and for how long each of its sound tracks runs against the
//! pictures.
//!
//! **What it cannot line up, it does not pretend to.** A join whose clips
//! cross by a transition has pictures that belong to neither side, and one
//! whose clips were conformed to the master's shape has pictures written
//! afresh: those are read for damage and length and nothing else, and
//! [`Report::compared`] says so. A file of sound alone is measured against
//! the ranges' length.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use crate::input::ReadPackets;
use crate::Source;

/// Below this, a re-encoded picture is taken for a different picture.
///
/// A re-encode at the quality the cutter writes sits well above 35 dB against
/// the picture it replaces; the next picture over, even in a still scene,
/// comes in far below it once there is any motion or a cut at all. The gap
/// between the two is wide, and this sits in the middle of it.
pub const LIKE_DB: f64 = 25.0;

/// How far the sound may run short of or past the pictures before it is
/// mentioned, in seconds. A cut carries whole audio frames, and a track may
/// start a frame or two later than the pictures; half a second is past
/// anything that comes of that.
pub const SOUND_SLACK: f64 = 0.5;

/// How closely the output can be lined up against its sources.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Compare {
    /// Picture by picture: the count, and each picture against its own.
    Pictures,
    /// The count and the damage, and not the pictures themselves. For a run
    /// that wrote every picture back smaller (see [`crate::fit`]): none of
    /// them is the recording's any more, and how far a shrunk picture may
    /// drift from its original is the shrinking's business, not a sign that
    /// it is the wrong picture.
    Count,
    /// Neither: a join that crosses by a transition, whose pictures belong to
    /// no one source.
    Nothing,
}

/// One recording of what was written out, and what of it was kept.
pub struct Piece<'a> {
    pub src: &'a Source,
    pub ranges: &'a [(f64, f64)],
    /// What the cut planned for those ranges, where the caller has it: where
    /// each range really begins and ends, and how many pictures it meant to
    /// re-encode -- every other one should come back bit for bit. Empty
    /// where there is no plan to hold the output to.
    pub plans: &'a [crate::RangePlan],
}

/// How long one of the output's sound tracks runs.
#[derive(Debug, Clone)]
pub struct Sound {
    /// Its stream index in the output.
    pub index: usize,
    pub seconds: f64,
    /// False for a track nothing could be decoded out of. The AC-3 a
    /// Blu-ray's TrueHD is wrapped around is declared as a stream of its own
    /// and has no packets of its own -- they arrive as the TrueHD's -- in the
    /// recording as much as in the cut. Not measured is not the same as
    /// nought seconds long, and it is not reported as that. A track the cut
    /// declared and wrote nothing into is refused by the cut itself.
    pub measured: bool,
    /// How much shorter than the pictures this track may rightly run:
    /// [`Report::sound_short`], and with it every piece of a join whose
    /// recording has no track in this one's place. The cut pairs a clip's
    /// tracks with the master's by their order and leaves a gap where a
    /// clip has fewer (see [`crate::conform::Fit::audio`]): a bilingual
    /// recording joined with a single-track one has a second track that
    /// stops where the first recording does, which is what was asked for.
    pub short: f64,
}

/// What reading the output back found.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Whether the output has pictures at all.
    pub pictures: bool,
    /// Whether it should have had them and has none: pictures were to be
    /// lined up or counted, and the output holds no video stream.
    pub pictures_lost: bool,
    /// Whether they were compared with the source's, one by one.
    pub compared: bool,
    /// Pictures decoded out of the output.
    pub produced: usize,
    /// Pictures the kept ranges hold in the source. Only where compared.
    pub expected: usize,
    /// Pictures the same as the source's, bit for bit.
    pub identical: usize,
    /// Pictures compared that were not identical: the re-encoded ones.
    pub reencoded: usize,
    /// How many of the source's pictures fall in a stretch the plans copy,
    /// how many of those did not come back bit for bit, and how many such
    /// stretches there are. Only where every piece came with its plan.
    pub planned_copy: Option<usize>,
    pub copy_misses: usize,
    pub stretches: usize,
    /// The output time of the first of those. Pictures are paired in order,
    /// so after one goes missing every pair after it is out by one: this is
    /// where to look.
    pub first_copy_miss: Option<f64>,
    /// How many of those misses lie away from every edge of a copied
    /// stretch, where there is no slack: a picture there that did not come
    /// back bit for bit was damaged on the way, not placed a frame out.
    pub inner_misses: usize,
    /// The lowest luma PSNR among those, in dB.
    pub worst_db: Option<f64>,
    /// How many of them came in under [`LIKE_DB`], and the output time of the
    /// first one.
    pub unlike: usize,
    pub first_unlike: Option<f64>,
    /// Pictures compared whose sizes differ, so that no PSNR could be taken.
    pub unmatched_size: usize,
    /// Pictures of the output the decoder flagged as damaged, and of the
    /// source's kept pictures. A recording that was damaged where it was
    /// kept passes its damage on, and that is not the cut's doing.
    pub damaged: usize,
    pub damaged_in_source: usize,
    /// Pictures compared that differ where a decoder was concealing damage,
    /// from a damaged picture up to the next pair that agrees bit for bit.
    /// Not held against the cut: see where they are counted.
    pub after_damage: usize,
    /// How long the pictures run, in seconds -- or, for sound alone, how long
    /// the ranges are.
    pub seconds: f64,
    /// Every sound track of the output.
    pub sounds: Vec<Sound>,
    /// How much shorter than [`Report::seconds`] the sound may rightly run:
    /// the part of the ranges the recordings' own sound does not reach. A
    /// broadcast's sound stops half a second or so before its last picture,
    /// and a recording with no sound at all adds nothing to a join's.
    pub sound_short: f64,
}

impl Report {
    /// Whether the pictures are what was asked for.
    pub fn pictures_ok(&self) -> bool {
        let counted = !self.compared || self.produced == self.expected;
        // Where the pictures are not lined up the source is never decoded,
        // and a recording's own reception errors come through a transition
        // join as much as a cut: damage there is said, and not held against
        // the output.
        let damage = !self.compared || self.damaged <= self.damaged_in_source;
        let present = !self.pictures || self.produced > 0;
        counted && damage && present && !self.pictures_lost && self.unlike == 0 && self.copies_came_back()
    }

    /// Whether the pictures the plan copied came back as the recording's own.
    ///
    /// The strong half of the check. A picture out of place by a frame or two
    /// in a scene that hardly moves is close enough to its neighbour to pass
    /// for it on a PSNR; it is not the same bits. So every picture that did
    /// not come back bit for bit has to be one the plan re-encoded.
    ///
    /// Asked picture by picture rather than of the plan's counts. A plan
    /// says how many frames a stretch holds from its length, and where the
    /// pictures are not evenly spaced -- a broadcast mixing pictures shown for
    /// three fields with ones shown for two, a phone's recording at 56.6
    /// frames a second -- that count is out by several a stretch on a cut
    /// that is exactly right. So each of the source's pictures is placed in
    /// the plan by its own time, and a picture that falls in a copied stretch
    /// is the one held to its bits. A picture's slack at each stretch is left
    /// for where the two part company at an edge.
    pub fn copies_came_back(&self) -> bool {
        self.planned_copy.is_none() || (self.inner_misses == 0 && self.copy_misses <= self.stretches)
    }

    /// The sound tracks that do not run as long as the pictures, with how
    /// far out they are (positive for longer).
    pub fn sound_off(&self) -> Vec<(usize, f64)> {
        self.sounds
            .iter()
            .filter(|s| s.measured)
            .map(|s| (s.index, s.seconds - self.seconds, s.short))
            .filter(|&(_, d, short)| d > SOUND_SLACK || d < -(short + SOUND_SLACK))
            .map(|(index, d, _)| (index, d))
            .collect()
    }

    /// Whether it all came out as asked.
    pub fn passed(&self) -> bool {
        self.pictures_ok() && self.sound_off().is_empty()
    }

    /// The report as the command line prints it.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.pictures_lost {
            out.push("frames   : none written, the output has no pictures  MISMATCH".to_string());
        }
        if self.pictures {
            if self.compared {
                out.push(format!(
                    "frames   : {} written, {} expected  {}",
                    self.produced,
                    self.expected,
                    if self.produced == self.expected { "OK" } else { "MISMATCH" }
                ));
                out.push(format!(
                    "identical: {} bit-identical to the source, {} re-encoded{}{}{}  {}",
                    self.identical,
                    self.reencoded,
                    self.planned_copy
                        .map(|p| format!(" ({} of {p} copied frames differ)", self.copy_misses))
                        .unwrap_or_default(),
                    self.first_copy_miss
                        .map(|t| format!(" (first at {t:.3}s)"))
                        .unwrap_or_default(),
                    self.worst_db
                        .map(|db| format!(" (lowest {db:.1} dB)"))
                        .unwrap_or_default(),
                    if self.copies_came_back() { "OK" } else { "MISMATCH" }
                ));
                if self.unlike > 0 {
                    out.push(format!(
                        "unlike   : {} frame(s) do not match the source's, first at {:.3}s  MISMATCH",
                        self.unlike,
                        self.first_unlike.unwrap_or(0.0)
                    ));
                }
            } else {
                out.push(format!(
                    "frames   : {} written (not compared with the source)",
                    self.produced
                ));
            }
            if self.damaged > 0 && self.compared {
                out.push(format!(
                    "damaged  : {} frame(s) decoded with errors ({} in the source's kept ranges)",
                    self.damaged, self.damaged_in_source
                ));
            } else if self.damaged > 0 {
                out.push(format!(
                    "damaged  : {} frame(s) decoded with errors (the source was not read to compare)",
                    self.damaged
                ));
            }
            if self.after_damage > 0 {
                out.push(format!(
                    "damaged  : {} frame(s) differ where the decoder was concealing damage (not counted)",
                    self.after_damage
                ));
            }
        }
        for s in &self.sounds {
            if !s.measured {
                out.push(format!("sound #{}: not decodable here, not measured", s.index));
                continue;
            }
            let d = s.seconds - self.seconds;
            let off = d > SOUND_SLACK || d < -(s.short + SOUND_SLACK);
            out.push(format!(
                "sound #{}: {:.3}s against {:.3}s{}  {}",
                s.index,
                s.seconds,
                self.seconds,
                // Not for a sliver that prints as nought.
                if s.short >= 0.0005 {
                    format!(" (the recording's own sound {:.3}s less)", s.short)
                } else {
                    String::new()
                },
                if off { "OFF" } else { "OK" }
            ));
        }
        out
    }
}

/// What the source side hands over for each picture.
struct Seen {
    /// Whether the plan copied this picture rather than re-encoding it.
    copied: bool,
    /// Whether it is close enough to an edge of its copied stretch to be
    /// the one picture there that may have been placed a frame out.
    edge: bool,
    hash: u64,
    grid: Grid,
    damaged: bool,
}

/// Luma on a sparse grid: every fourth sample of every fourth line.
struct Grid {
    width: u32,
    height: u32,
    samples: Vec<u8>,
}

impl Grid {
    fn empty() -> Grid {
        Grid { width: 0, height: 0, samples: Vec::new() }
    }

    fn of(frame: &ff::frame::Video) -> Grid {
        let luma = crate::Luma8::of(frame);
        let (w, h) = (frame.width() as usize, frame.height() as usize);
        let mut samples = Vec::with_capacity(w.div_ceil(4) * h.div_ceil(4));
        for y in (0..h).step_by(4) {
            for x in (0..w).step_by(4) {
                samples.push(luma.at(x, y));
            }
        }
        Grid { width: frame.width(), height: frame.height(), samples }
    }

    /// Luma PSNR against another grid of the same picture size, in dB.
    fn psnr(&self, other: &Grid) -> Option<f64> {
        if self.width != other.width
            || self.height != other.height
            || self.samples.len() != other.samples.len()
            || self.samples.is_empty()
        {
            return None;
        }
        let sum: u64 = self
            .samples
            .iter()
            .zip(&other.samples)
            .map(|(&a, &b)| {
                let d = a.abs_diff(b) as u64;
                d * d
            })
            .sum();
        let mse = sum as f64 / self.samples.len() as f64;
        Some(if mse == 0.0 { 99.0 } else { 10.0 * (255.0 * 255.0 / mse).log10() })
    }
}

/// A hash of every sample of a picture, row by row and plane by plane.
///
/// Not a cryptographic one and not meant to be: two pictures are being asked
/// whether they are the same decode of the same bits, and the thing to avoid
/// is taking half the run's time over it. A word at a time.
fn picture_hash(frame: &ff::frame::Video) -> u64 {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut h: u64 = u64::from(frame.width()) << 32 | u64::from(frame.height());
    let format: ff::ffi::AVPixelFormat = frame.format().into();
    for plane in 0..frame.planes() {
        let row = unsafe { ff::ffi::av_image_get_linesize(format, frame.width() as i32, plane as i32) };
        let row = row.max(0) as usize;
        let stride = frame.stride(plane);
        let data = frame.data(plane);
        let rows = frame.plane_height(plane) as usize;
        for y in 0..rows {
            let start = y * stride;
            let Some(line) = data.get(start..start + row.min(stride)) else { break };
            // `as_chunks` would say this more plainly, and is newer than the
            // Rust this workspace promises to build with.
            #[allow(clippy::chunks_exact_to_as_chunks)]
            let mut words = line.chunks_exact(8);
            for w in &mut words {
                let v = u64::from_le_bytes(w.try_into().unwrap_or([0; 8]));
                h = (h.rotate_left(5) ^ v).wrapping_mul(K);
            }
            for &b in words.remainder() {
                h = (h.rotate_left(5) ^ u64::from(b)).wrapping_mul(K);
            }
        }
    }
    h
}

/// Whether the decoder says it had to paper over damage in this picture.
// The flag's type is whatever bindgen made of a `#define`, which is not the
// same on every platform this is built for.
#[allow(clippy::unnecessary_cast)]
fn damaged(frame: &ff::frame::Video) -> bool {
    unsafe {
        let f = frame.as_ptr();
        ((*f).flags & ff::ffi::AV_FRAME_FLAG_CORRUPT as i32) != 0 || (*f).decode_error_flags != 0
    }
}

/// Read `output` back and check it against `pieces`, the recordings it was
/// cut from in the order it holds them.
///
/// `compare` is how closely to line the two up; see [`Compare`]. It comes
/// down to [`Compare::Nothing`] here where the pieces do not share one
/// picture shape. `told` hears how far through the output the read
/// has got, as a fraction; `stop` ends it early with an error.
pub fn check(
    output: &str,
    pieces: &[Piece],
    compare: Compare,
    told: Option<Box<dyn FnMut(f64) + Send>>,
    stop: &AtomicBool,
) -> Result<Report> {
    check_crossed(output, pieces, &[], compare, told, stop)
}

/// [`check`], for a file of sound alone joined with transitions: `after[n]`
/// is what follows piece `n`, as [`crate::sound::Piece::after`] has it. A
/// crossing that overlaps the two pieces' sound makes the file that much
/// shorter than the ranges, and the length it is held to is shortened the
/// same way. Pictures are measured off the output and need none of it.
pub fn check_crossed(
    output: &str,
    pieces: &[Piece],
    after: &[crate::transition::Transition],
    compare: Compare,
    told: Option<Box<dyn FnMut(f64) + Send>>,
    stop: &AtomicBool,
) -> Result<Report> {
    check_placed(output, pieces, after, None, compare, told, stop)
}

/// [`check_crossed`], told where the output's sound tracks sit among the
/// master's: `places[k]` is the position in the master's
/// [`Source::audios`] of the output's `k`th track. A join pairs a clip's
/// tracks with the master's by that position, not by the order they come
/// out in: a master whose first track was left out writes its second as
/// the output's first, and a clip with one track has nothing for it -- the
/// track stops where the master does, which is what was asked for. `None`
/// takes the output's order for the master's, which is right whenever no
/// track of the master was left out.
pub fn check_placed(
    output: &str,
    pieces: &[Piece],
    after: &[crate::transition::Transition],
    places: Option<&[usize]>,
    compare: Compare,
    mut told: Option<Box<dyn FnMut(f64) + Send>>,
    stop: &AtomicBool,
) -> Result<Report> {
    crate::init()?;
    let mut ictx = crate::input::demux(output)?;
    let video = ictx.streams().best(ff::media::Type::Video).map(|s| s.index());
    let audio: Vec<usize> = ictx
        .streams()
        .filter(|s| s.parameters().medium() == ff::media::Type::Audio)
        .map(|s| s.index())
        .collect();
    let mut report = Report {
        pictures: video.is_some(),
        pictures_lost: video.is_none() && compare != Compare::Nothing,
        ..Default::default()
    };

    // What the ranges come to, which is what a file of sound alone should run
    // for, and what the bar below is drawn against.
    //
    // Each range as far as the recording goes: a range asked for past its
    // end is cut as far as there is anything, and so is the sound alone.
    let within = |p: &Piece, &(a, b): &(f64, f64)| {
        let end = if p.src.duration > 0.0 { b.min(p.src.duration) } else { b };
        (end - a.max(0.0)).max(0.0)
    };
    let kept: f64 = pieces
        .iter()
        .flat_map(|p| p.ranges.iter().map(move |r| within(p, r)))
        .sum::<f64>()
        - crossed(pieces, after);
    // How much of that the recordings' own sound does not cover. See
    // [`Report::sound_short`].
    let mut sound_short = 0.0;
    // And what each track lacks at a recorder's seams inside the ranges, per
    // piece, by the track's place among that piece's own. See
    // [`seam_missing`].
    let mut seam_short: Vec<Vec<f64>> = Vec::new();
    for p in pieces {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let (missing, span) = sound_missing(p, &within);
        sound_short += missing;
        seam_short.push(seam_missing(p, span));
    }
    let fps = pieces.first().map(|p| p.src.video.frame_rate).unwrap_or(0.0);
    let estimate = (kept * if fps > 0.0 { fps } else { 30.0 }).max(1.0);

    // A join whose clips differ in the pictures -- size, rate, codec, the
    // things `conform` rewrites a clip over -- has clips that were written
    // afresh in the master's shape, which are not their recordings' pictures
    // any more. Every pair is asked, not each clip against the first: the
    // question is not transitive (a clip silent about its colours fits one
    // that says bt709 and one that says bt470bg, which do not fit each
    // other), and the cut asks it of the master, which need not be first.
    let shaped_alike = pieces.iter().enumerate().all(|(i, a)| {
        pieces[i + 1..].iter().all(|b| !crate::conform::fit(a.src, b.src).video)
    });
    report.compared = compare != Compare::Nothing && video.is_some() && !pieces.is_empty() && shaped_alike;
    // Held to the plans only where the pictures are compared as pictures:
    // a run that wrote them all back smaller copied none of them.
    let held = report.compared
        && compare == Compare::Pictures
        && pieces.iter().all(|p| !p.plans.is_empty());
    if held {
        report.planned_copy = Some(0);
        report.stretches = pieces
            .iter()
            .flat_map(|p| p.plans.iter())
            .flat_map(|r| r.segments.iter())
            .filter(|s| s.kind == crate::SegmentKind::Copy)
            .count();
    }

    let mut decoder = match video {
        Some(i) => {
            let params = ictx.stream(i).ok_or_else(|| anyhow!("stream {i} vanished"))?.parameters();
            // As the source side decodes: see [`crate::preview::steady_decoder`].
            Some(crate::preview::steady_decoder(params)?)
        }
        None => None,
    };
    // Stream, decoder, packets read, seconds decoded.
    let mut sounds: Vec<(usize, ff::decoder::Audio, u64, f64)> = Vec::new();
    for &i in &audio {
        let Some(stream) = ictx.stream(i) else { continue };
        let Ok(ctx) = ff::codec::context::Context::from_parameters(stream.parameters()) else {
            continue;
        };
        // A track no decoder here can read is one that cannot be measured,
        // which is not the same as one that is wrong: left out rather than
        // reported at nought seconds.
        let Ok(dec) = ctx.decoder().audio() else { continue };
        sounds.push((i, dec, 0, 0.0));
    }
    let v_tb = video.and_then(|i| ictx.stream(i)).map(|s| f64::from(s.time_base())).unwrap_or(0.0);
    // Where the output's clock starts, which is where a player counts from:
    // a time said in the report is one to go and look at. A transport stream
    // written here opens at 0.05s and an .m2ts at 0.85s, and the raw times
    // put the first mismatch that far past where the player shows it.
    let origin = crate::container_start(&ictx);
    // Why the output could not be read to its end, where it could not.
    let mut unread: Option<anyhow::Error> = None;

    // The source side, on a thread of its own, a picture at a time through a
    // short queue.
    let (tx, rx) = mpsc::sync_channel::<Seen>(32);
    // Where each range really begins and ends. The planner moves an end onto
    // an access point within half a frame of it, so a range asked for from
    // 181.100 is written from the picture at 181.097 -- which, held to the
    // times as asked, is a picture the output has and the source side does
    // not, and every picture after it one out. The plan's own ends where
    // there is a plan.
    //
    // With each range, the stretches of it the plan copies: a picture there
    // is one that should come back bit for bit.
    type Range = (f64, f64, Option<f64>, Vec<(f64, f64)>);
    let owned: Vec<(Source, Vec<Range>)> = if report.compared {
        pieces
            .iter()
            .map(|p| {
                // A plan need not have a range for each one asked for: one
                // that crosses a seam of a recorder's clip is planned as two,
                // and the pictures between the seam and the next entry
                // point, which cannot be decoded, are left out of the cut.
                // The plans are what was written, whenever there are any.
                //
                // A plan with no segments wrote nothing: a range thinner
                // than a picture, or one [`crate::plan::plan_on`] emptied
                // because it lay between a recorder's seam and the next
                // entry point. Asked for its pictures anyway, the source
                // side handed on the picture still up from before it --
                // across the seam, the last of the previous stretch, which
                // the range before already holds -- and the check counted
                // one picture more than the cut wrote.
                let ranges = if !p.plans.is_empty() {
                    p.plans
                        .iter()
                        .filter(|r| !r.segments.is_empty())
                        .map(|r| {
                            let copies = r
                                .segments
                                .iter()
                                .filter(|s| s.kind == crate::SegmentKind::Copy)
                                .map(|s| (s.start, s.end))
                                .collect();
                            // Where the range opens on a re-encode, where that
                            // ends: see [`crate::preview::pictures_in`].
                            let head = r
                                .segments
                                .first()
                                .filter(|s| s.kind == crate::SegmentKind::Reencode)
                                .map(|s| s.end);
                            (r.t_in, r.t_out, head, copies)
                        })
                        .collect()
                } else {
                    p.ranges.iter().map(|&(a, b)| (a, b, None, Vec::new())).collect()
                };
                (p.src.clone(), ranges)
            })
            .collect()
    } else {
        Vec::new()
    };

    std::thread::scope(|scope| -> Result<()> {
        let source_side = scope.spawn(move || -> Result<()> {
            for (src, ranges) in &owned {
                for (a, b, head, copies) in ranges {
                    let mut gone = false;
                    // The grid is only read where the pictures are compared.
                    let grids = compare == Compare::Pictures;
                    // Two frames either side of an edge: where uneven pictures
                    // put the plan's time a frame from the picture's.
                    let near = 2.0 * src.video.frame_duration();
                    crate::preview::pictures_in(src, *a, *b, *head, |t, frame| {
                        let stretch = copies.iter().find(|&&(s, e)| t >= s - 1e-4 && t < e - 1e-4);
                        let seen = Seen {
                            hash: picture_hash(frame),
                            grid: if grids { Grid::of(frame) } else { Grid::empty() },
                            damaged: damaged(frame),
                            copied: stretch.is_some(),
                            edge: stretch.is_some_and(|&(s, e)| t - s < near || e - t < near),
                        };
                        if tx.send(seen).is_err() {
                            gone = true;
                            return false;
                        }
                        !stop.load(Ordering::Relaxed)
                    })?;
                    if gone || stop.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                }
            }
            Ok(())
        });

        let mut frame = ff::frame::Video::empty();
        let mut sound = ff::frame::Audio::empty();
        let mut first_pts: Option<i64> = None;
        let mut last_pts: Option<i64> = None;
        let mut said = 0.0;
        let mut tainted = false;
        let mut on_picture = |frame: &ff::frame::Video, report: &mut Report| {
            // The best guess where there is no presentation time: Matroska
            // holds VC-1 by its decode times alone, and every picture of one
            // comes out of the decoder without a pts.
            let pts = frame.pts().or_else(|| frame.timestamp());
            let at = pts.map(|p| (p as f64 * v_tb - origin).max(0.0)).unwrap_or(0.0);
            if let Some(p) = pts {
                first_pts = Some(first_pts.map_or(p, |f: i64| f.min(p)));
                last_pts = Some(last_pts.map_or(p, |l: i64| l.max(p)));
            }
            report.produced += 1;
            let broken = damaged(frame);
            if broken {
                report.damaged += 1;
            }
            if report.compared {
                if let Ok(seen) = rx.recv() {
                    report.expected += 1;
                    if seen.damaged {
                        report.damaged_in_source += 1;
                    }
                    let same = seen.hash == picture_hash(frame);
                    // Where either decoder has had to conceal damage, what it
                    // makes of the pictures after it depends on what it had
                    // decoded before -- and the two sides decoded different
                    // things before a copy's first picture. Until the two
                    // agree again, a difference is the damage's, not the cut's.
                    if seen.damaged || broken {
                        tainted = true;
                    } else if same {
                        tainted = false;
                    }
                    if !same && tainted {
                        report.after_damage += 1;
                    }
                    if seen.copied {
                        if let Some(n) = report.planned_copy.as_mut() {
                            *n += 1;
                            if !same && !tainted {
                                report.copy_misses += 1;
                                report.first_copy_miss.get_or_insert(at);
                                if !seen.edge {
                                    report.inner_misses += 1;
                                }
                            }
                        }
                    }
                    if same {
                        report.identical += 1;
                    } else if compare == Compare::Count {
                        report.reencoded += 1;
                    } else {
                        report.reencoded += 1;
                        match seen.grid.psnr(&Grid::of(frame)) {
                            Some(db) => {
                                report.worst_db = Some(report.worst_db.map_or(db, |w| w.min(db)));
                                if db < LIKE_DB && !tainted {
                                    report.unlike += 1;
                                    report.first_unlike.get_or_insert(at);
                                }
                            }
                            None => report.unmatched_size += 1,
                        }
                    }
                }
            }
            let f = report.produced as f64 / estimate;
            if let Some(t) = told.as_mut() {
                if f - said >= 0.002 {
                    said = f;
                    t(f.min(1.0));
                }
            }
        };

        let mut packets = ictx.read_packets();
        for (stream, packet) in packets.by_ref() {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let i = stream.index();
            if Some(i) == video {
                let Some(dec) = decoder.as_mut() else { continue };
                if dec.send_packet(&packet).is_err() {
                    // A packet the decoder turns away. Where the pictures are
                    // lined up, a picture it cost is one the count is short
                    // of -- and the source side's decoder turns away the same
                    // packet of a recording damaged there, which is not
                    // counted as damage on that side either. Counted here only
                    // where nothing else would notice.
                    if !report.compared {
                        report.damaged += 1;
                    }
                    continue;
                }
                while dec.receive_frame(&mut frame).is_ok() {
                    on_picture(&frame, &mut report);
                }
            } else if let Some(s) = sounds.iter_mut().find(|s| s.0 == i) {
                s.2 += 1;
                if s.1.send_packet(&packet).is_ok() {
                    while s.1.receive_frame(&mut sound).is_ok() {
                        if sound.rate() > 0 {
                            s.3 += sound.samples() as f64 / f64::from(sound.rate());
                        }
                    }
                }
            }
        }
        // A read that gave up at errors -- a share that went away -- is not
        // a file that is short of pictures: counted as one, the check said a
        // cut it never read to the end had lost them. Said as what it is,
        // once the source side has been let finish below.
        unread = packets.finished().err();
        drop(packets);
        if let Some(dec) = decoder.as_mut() {
            let _ = dec.send_eof();
            while dec.receive_frame(&mut frame).is_ok() {
                on_picture(&frame, &mut report);
            }
        }
        for s in sounds.iter_mut() {
            let _ = s.1.send_eof();
            while s.1.receive_frame(&mut sound).is_ok() {
                if sound.rate() > 0 {
                    s.3 += sound.samples() as f64 / f64::from(sound.rate());
                }
            }
        }
        // Whatever the source still has queued is pictures the output does
        // not have. Counted, which also lets the source side finish -- but
        // not after an output that could not be read to its end: the count
        // is not going to be reported, and draining decoded the rest of
        // every range of the source before saying so.
        if report.compared && !stop.load(Ordering::Relaxed) && unread.is_none() {
            while let Ok(seen) = rx.recv() {
                report.expected += 1;
                if seen.damaged {
                    report.damaged_in_source += 1;
                }
            }
        } else {
            drop(rx);
        }
        report.seconds = if report.pictures {
            match (first_pts, last_pts) {
                (Some(f), Some(l)) => (l - f) as f64 * v_tb + 1.0 / frame_rate(&ictx, video, fps),
                _ => 0.0,
            }
        } else {
            kept
        };
        report.sound_short = sound_short;
        // Each track's place among the master's, for [`Sound::short`]:
        // counted over the tracks that had packets of their own, since the
        // AC-3 a TrueHD track is wrapped around is declared as a stream of
        // its own and is no track of the master's.
        let mut place = 0usize;
        // Whether the output's tracks can be taken for a piece's own by
        // place: told so, or every one of the piece's tracks is here. Where
        // one was left out and nobody said which, a seam's hole is held to
        // the track that lacks the most -- the head and the tail are held to
        // all of them that way too.
        let written = sounds.iter().filter(|s| s.2 > 0).count();
        report.sounds = sounds
            .iter()
            .map(|s| {
                let at = places.map_or(place, |p| p.get(place).copied().unwrap_or(place));
                if s.2 > 0 {
                    place += 1;
                }
                // A recording with no sound at all is in `sound_short`
                // already.
                let gap: f64 = pieces
                    .iter()
                    .filter(|p| !p.src.audios.is_empty() && p.src.audios.len() <= at)
                    .flat_map(|p| p.ranges.iter().map(move |r| within(p, r)))
                    .sum();
                let seams: f64 = pieces
                    .iter()
                    .zip(&seam_short)
                    .map(|(p, holes)| {
                        if places.is_some() || written >= p.src.audios.len() {
                            holes.get(at).copied().unwrap_or(0.0)
                        } else {
                            holes.iter().fold(0.0, |m: f64, &h| m.max(h))
                        }
                    })
                    .sum();
                Sound {
                    index: s.0,
                    seconds: s.3,
                    measured: s.3 > 0.0,
                    short: sound_short + gap + seams,
                }
            })
            .collect();
        source_side.join().map_err(|_| anyhow!("the source side of the check stopped"))??;
        Ok(())
    })?;

    if stop.load(Ordering::Relaxed) {
        return Err(anyhow!("stopped"));
    }
    if let Some(e) = unread {
        return Err(anyhow!("reading {output} back: {e}"));
    }
    if let Some(t) = told.as_mut() {
        t(1.0);
    }
    Ok(report)
}

/// How long the crossings that overlap two pieces' sound take out of it.
///
/// The arithmetic of `overlapped` in [`crate::sound`], which writes the file:
/// each side gives at most half its range, and nothing follows the last.
fn crossed(pieces: &[Piece], after: &[crate::transition::Transition]) -> f64 {
    let room = |r: Option<&(f64, f64)>| r.map_or(0.0, |&(a, b)| ((b - a) / 2.0).max(0.0));
    (0..pieces.len().saturating_sub(1))
        .filter_map(|n| Some((n, after.get(n)?)))
        .filter(|(_, t)| t.kind.overlaps())
        .map(|(n, t)| {
            t.takes()
                .1
                .min(room(pieces[n].ranges.last()))
                .min(room(pieces[n + 1].ranges.first()))
                .max(0.0)
        })
        .sum()
}

/// How much of a piece's ranges its recording has no sound for.
///
/// All of them for a recording with no sound track. Otherwise what lies
/// before the latest of its tracks' first sound and after the earliest of
/// their last: read off the file's first and last few seconds, and only for
/// a piece with a range near either end -- in between, the sound runs, bar
/// a recorder's seams, which [`seam_missing`] measures.
///
/// Comes back with the span it measured, `(0, inf)` where it measured none,
/// so that what lies outside it is not counted a second time at a seam.
fn sound_missing(
    p: &Piece,
    within: &dyn Fn(&Piece, &(f64, f64)) -> f64,
) -> (f64, (f64, f64)) {
    const NEAR: f64 = 10.0;
    let unmeasured = (0.0, f64::INFINITY);
    let whole: f64 = p.ranges.iter().map(|r| within(p, r)).sum();
    if p.src.audios.is_empty() {
        return (whole, unmeasured);
    }
    let duration = p.src.duration;
    let head = p.ranges.iter().any(|&(a, _)| a < NEAR);
    let tail = duration > 0.0 && p.ranges.iter().any(|&(_, b)| b > duration - NEAR);
    if !head && !tail {
        return (0.0, unmeasured);
    }
    let Some((from, to)) = sound_span(p.src, head, tail, NEAR) else { return (0.0, unmeasured) };
    // What of each range lies inside the span.
    let inside: f64 = p
        .ranges
        .iter()
        .map(|r| {
            let end = if duration > 0.0 { r.1.min(duration) } else { r.1 };
            (end.min(to) - r.0.max(0.0).max(from)).max(0.0)
        })
        .sum();
    ((whole - inside).max(0.0), (from, to))
}

/// How much of a piece's ranges each of its sound tracks has no sound for at
/// the seams of a recorder's clip, by the track's place in
/// [`Source::audios`].
///
/// **A recorder's clip can lose seconds of one track where it was paused.**
/// On one BD-RE recorder's clip the second track stops 3.2 s before a seam
/// that the pictures and the first track run straight through, and a cut
/// across it, which carries the recording faithfully, was reported as that
/// track running short. [`sound_missing`] only reads the two ends of the
/// file, so each seam a range reaches is read as well: the last sound before
/// it and the first after, track by track. Only what the recording lacks is
/// allowed for: a track the cut loses where the recording has it is still
/// reported. Where a read cannot say -- no sound of a track on one side
/// within the stretch read -- the hole is taken as only as long as what was
/// read, so that what is allowed for is never more than was measured.
///
/// `span` is where [`sound_missing`] found the sound running; what lies
/// outside it is counted there already.
fn seam_missing(p: &Piece, span: (f64, f64)) -> Vec<f64> {
    /// How far before a seam the read begins, and how far after it the first
    /// sound of each track is looked for. The ends of the file are read as
    /// far ([`sound_missing`]'s `NEAR`), and the hole that was measured is
    /// 3.2 s.
    const BACK: f64 = 10.0;
    const ON: f64 = 10.0;
    let src = p.src;
    let mut out = vec![0.0; src.audios.len()];
    if out.is_empty() || src.joins.is_empty() {
        return out;
    }
    let duration = src.duration;
    let ranges: Vec<(f64, f64)> = p
        .ranges
        .iter()
        .map(|&(a, b)| {
            let end = if duration > 0.0 { b.min(duration) } else { b };
            (a.max(0.0).max(span.0), end.min(span.1))
        })
        .filter(|(a, b)| b > a)
        .collect();
    for (n, j) in src.joins.iter().enumerate() {
        let next = src.joins.get(n + 1);
        let until = next.map_or(j.time + ON, |x| x.ends.min(j.time + ON));
        if !ranges.iter().any(|&(a, b)| a < until && b > j.ends - BACK) {
            continue;
        }
        let Some(holes) = seam_holes(src, j, next, BACK, until) else { continue };
        for (k, hole) in holes.into_iter().enumerate() {
            out[k] += hole_in(&ranges, hole, (j.ends, j.time));
        }
    }
    out
}

/// How much of `ranges` a track's `hole` covers, leaving out `between`: the
/// stretch from where the pictures before a seam end to where the ones after
/// it begin, which the pictures lack as much as the sound does.
fn hole_in(ranges: &[(f64, f64)], (h0, h1): (f64, f64), (ends, time): (f64, f64)) -> f64 {
    let over = |x0: f64, x1: f64| -> f64 {
        ranges.iter().map(|&(a, b)| (b.min(x1) - a.max(x0)).max(0.0)).sum()
    };
    if h1 <= h0 {
        return 0.0;
    }
    over(h0, h1.min(ends)) + over(h0.max(time.max(ends)), h1)
}

/// Each sound track's hole at one seam: from the end of its last packet
/// before the seam to the start of its first after it. Read by byte, as the
/// cut reads a stretch (see [`crate::restamp::Seam`]): over a seam the times
/// need not climb with the bytes, so which side a packet is on is its
/// position. `None` where the recording cannot be read.
fn seam_holes(
    src: &Source,
    j: &crate::restamp::Seam,
    next: Option<&crate::restamp::Seam>,
    back: f64,
    until: f64,
) -> Option<Vec<(f64, f64)>> {
    let tracks: Vec<(usize, f64)> =
        src.audios.iter().map(|a| (a.stream_index, a.time_base)).collect();
    let mut ictx = crate::input::demux(&src.input.url).ok()?;
    let want = j.ends - back;
    // The last entry point of the stretch before that is far enough ahead.
    let landing = src
        .points
        .iter()
        .rfind(|p| p.pos >= 0 && (p.pos as u64) < j.at && p.time <= want);
    let placed = match landing {
        Some(p) if src.byte_seekable => unsafe {
            ff::ffi::av_seek_frame(ictx.as_mut_ptr(), -1, p.pos, ff::ffi::AVSEEK_FLAG_BYTE) >= 0
        },
        _ => false,
    };
    if !placed {
        let target = ((want.max(0.0) + src.start_time) * ff::ffi::AV_TIME_BASE as f64) as i64;
        ictx.seek(target, ..target).ok()?;
    }
    let wall = next.map(|x| x.at);
    let mut read_at: Option<u64> = None;
    // The earliest time read before the seam: where a track has nothing
    // before it, its hole is known to reach back at least that far.
    let mut opened: Option<f64> = None;
    let mut last: Vec<Option<f64>> = vec![None; tracks.len()];
    let mut first: Vec<Option<f64>> = vec![None; tracks.len()];
    let mut reading = ictx.read_packets();
    for (stream, packet) in reading.by_ref() {
        if packet.position() >= 0 {
            read_at = Some(packet.position() as u64);
        }
        let Some(at) = read_at else { continue };
        if wall.is_some_and(|w| at >= w) {
            break;
        }
        let k = tracks.iter().position(|t| t.0 == stream.index());
        let tb = k.map_or_else(|| f64::from(stream.time_base()), |k| tracks[k].1);
        let Some(t) = packet.pts().map(|p| p as f64 * tb - src.start_time) else { continue };
        if at < j.at {
            opened = Some(opened.map_or(t, |o: f64| o.min(t)));
            if let Some(k) = k {
                let end = t + packet.duration().max(0) as f64 * tb;
                last[k] = Some(last[k].map_or(end, |l: f64| l.max(end)));
            }
        } else {
            if let Some(k) = k {
                first[k] = Some(first[k].map_or(t, |f: f64| f.min(t)));
            }
            if t > until || first.iter().all(Option::is_some) {
                break;
            }
        }
    }
    // A read that stopped on an error says nothing it can be held to.
    reading.finished().ok()?;
    let opened = opened.unwrap_or(j.ends).max(j.ends - back);
    Some(
        last.iter()
            .zip(&first)
            .map(|(l, f)| (l.unwrap_or(opened), f.unwrap_or(until)))
            .collect(),
    )
}

/// Where a recording's sound begins and ends, as the latest first packet and
/// the earliest last one among its tracks. `None` where it cannot be read.
fn sound_span(src: &Source, head: bool, tail: bool, near: f64) -> Option<(f64, f64)> {
    let tracks: Vec<(usize, f64)> =
        src.audios.iter().map(|a| (a.stream_index, a.time_base)).collect();
    let time = |packet: &ff::Packet, tb: f64| {
        packet.pts().map(|p| p as f64 * tb - src.start_time)
    };
    let mut from = 0.0_f64;
    if head {
        let mut ictx = crate::input::demux(&src.input.url).ok()?;
        let mut first: Vec<Option<f64>> = vec![None; tracks.len()];
        for (stream, packet) in ictx.read_packets() {
            let Some(k) = tracks.iter().position(|t| t.0 == stream.index()) else {
                if time(&packet, f64::from(stream.time_base())).is_some_and(|t| t > near) {
                    break;
                }
                continue;
            };
            let Some(t) = time(&packet, tracks[k].1) else { continue };
            if first[k].is_none() {
                first[k] = Some(t);
            }
            if t > near || first.iter().all(Option::is_some) {
                break;
            }
        }
        from = first.iter().flatten().fold(0.0, |m, &t| m.max(t));
    }
    let mut to = f64::INFINITY;
    if tail && src.duration > 0.0 {
        let mut ictx = crate::input::demux(&src.input.url).ok()?;
        let target = ((src.duration - near).max(0.0) + src.start_time) * ff::ffi::AV_TIME_BASE as f64;
        let target = target as i64;
        ictx.seek(target, ..target).ok()?;
        let mut last: Vec<Option<f64>> = vec![None; tracks.len()];
        for (stream, packet) in ictx.read_packets() {
            let Some(k) = tracks.iter().position(|t| t.0 == stream.index()) else { continue };
            let Some(t) = time(&packet, tracks[k].1) else { continue };
            let end = t + packet.duration().max(0) as f64 * tracks[k].1;
            last[k] = Some(last[k].map_or(end, |l: f64| l.max(end)));
        }
        // A track with nothing near the end has stopped before it, and the
        // reading says nothing about where: left out rather than guessed.
        to = last.iter().flatten().fold(f64::INFINITY, |m, &t| m.min(t));
    }
    Some((from, to))
}

/// The output's frame rate, for the length of its last picture.
fn frame_rate(ictx: &crate::input::Demux, video: Option<usize>, fallback: f64) -> f64 {
    let r = video
        .and_then(|i| ictx.stream(i))
        .map(|s| f64::from(s.avg_frame_rate()))
        .filter(|r| r.is_finite() && *r > 0.0);
    r.unwrap_or(if fallback > 0.0 { fallback } else { 30.0 })
}

#[cfg(test)]
mod tests {
    use super::hole_in;

    /// A track that stops 3.2 s before a seam the pictures run straight
    /// through: the hole is allowed for where a range covers it, and only
    /// there.
    #[test]
    fn a_hole_before_a_seam_counts_where_kept() {
        let seam = (2730.0, 2730.0);
        let hole = (2726.8, 2730.02);
        let across = [(2700.0, 2760.0)];
        assert!((hole_in(&across, hole, seam) - 3.22).abs() < 1e-9);
        // A range ending inside the hole has only its part of it.
        let into = [(2700.0, 2728.0)];
        assert!((hole_in(&into, hole, seam) - 1.2).abs() < 1e-9);
        // Two ranges, one either side, and none of the hole between them.
        let around = [(2700.0, 2726.0), (2731.0, 2760.0)];
        assert_eq!(hole_in(&around, hole, seam), 0.0);
        // No hole: the sound runs on to the seam.
        assert_eq!(hole_in(&across, (2730.0, 2730.0), seam), 0.0);
    }

    /// Where the stretch before a seam ends earlier than the next begins, the
    /// pictures lack that stretch as well, and it is not the sound's to make
    /// up.
    #[test]
    fn a_gap_the_pictures_share_is_not_counted() {
        let seam = (100.0, 101.0);
        let across = [(90.0, 110.0)];
        assert!((hole_in(&across, (99.0, 101.5), seam) - 1.5).abs() < 1e-9);
        assert_eq!(hole_in(&across, (100.2, 100.8), seam), 0.0);
    }
}
