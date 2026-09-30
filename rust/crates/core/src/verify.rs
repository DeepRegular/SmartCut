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
}

/// What reading the output back found.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Whether the output has pictures at all.
    pub pictures: bool,
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
    /// How long the pictures run, in seconds -- or, for sound alone, how long
    /// the ranges are.
    pub seconds: f64,
    /// Every sound track of the output.
    pub sounds: Vec<Sound>,
}

impl Report {
    /// Whether the pictures are what was asked for.
    pub fn pictures_ok(&self) -> bool {
        let counted = !self.compared || self.produced == self.expected;
        let damage = if self.compared {
            self.damaged <= self.damaged_in_source
        } else {
            self.damaged == 0
        };
        let present = !self.pictures || self.produced > 0;
        counted && damage && present && self.unlike == 0 && self.copies_came_back()
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
        self.planned_copy.is_none() || self.copy_misses <= self.stretches
    }

    /// The sound tracks that do not run as long as the pictures, with how
    /// far out they are (positive for longer).
    pub fn sound_off(&self) -> Vec<(usize, f64)> {
        self.sounds
            .iter()
            .filter(|s| s.measured)
            .map(|s| (s.index, s.seconds - self.seconds))
            .filter(|&(_, d)| d.abs() > SOUND_SLACK)
            .collect()
    }

    /// Whether it all came out as asked.
    pub fn passed(&self) -> bool {
        self.pictures_ok() && self.sound_off().is_empty()
    }

    /// The report as the command line prints it.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.pictures {
            if self.compared {
                out.push(format!(
                    "frames   : {} written, {} expected  {}",
                    self.produced,
                    self.expected,
                    if self.produced == self.expected { "OK" } else { "MISMATCH" }
                ));
                out.push(format!(
                    "identical: {} bit-identical to the source, {} re-encoded{}{}  {}",
                    self.identical,
                    self.reencoded,
                    self.planned_copy
                        .map(|p| format!(" ({} of {p} copied frames differ)", self.copy_misses))
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
            if self.damaged > 0 {
                out.push(format!(
                    "damaged  : {} frame(s) decoded with errors ({} in the source's kept ranges)",
                    self.damaged, self.damaged_in_source
                ));
            }
        }
        for s in &self.sounds {
            if !s.measured {
                out.push(format!("sound #{}: not decodable here, not measured", s.index));
                continue;
            }
            let d = s.seconds - self.seconds;
            out.push(format!(
                "sound #{}: {:.3}s against {:.3}s  {}",
                s.index,
                s.seconds,
                self.seconds,
                if d.abs() > SOUND_SLACK { "OFF" } else { "OK" }
            ));
        }
        out
    }
}

/// What the source side hands over for each picture.
struct Seen {
    /// Whether the plan copied this picture rather than re-encoding it.
    copied: bool,
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
    let mut report = Report { pictures: video.is_some(), ..Default::default() };

    // What the ranges come to, which is what a file of sound alone should run
    // for, and what the bar below is drawn against.
    let kept: f64 = pieces
        .iter()
        .flat_map(|p| p.ranges.iter())
        .map(|&(a, b)| (b - a).max(0.0))
        .sum();
    let fps = pieces.first().map(|p| p.src.video.frame_rate).unwrap_or(0.0);
    let estimate = (kept * if fps > 0.0 { fps } else { 30.0 }).max(1.0);

    // A join whose clips differ in the pictures -- size, rate, codec, the
    // things `conform` rewrites a clip over -- has clips that were written
    // afresh in the master's shape, which are not their recordings' pictures
    // any more. Whichever clip is the master, one that differs from the first
    // differs from some clip of the list.
    let shaped_alike = pieces
        .iter()
        .skip(1)
        .all(|p| !crate::conform::fit(pieces[0].src, p.src).video);
    report.compared = compare != Compare::Nothing && video.is_some() && !pieces.is_empty() && shaped_alike;
    // Held to the plans only where the pictures are compared as pictures:
    // a run that wrote them all back smaller copied none of them.
    let held = report.compared
        && compare == Compare::Pictures
        && pieces.iter().all(|p| p.plans.len() == p.ranges.len() && !p.plans.is_empty());
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
            Some(crate::video_decoder(params)?)
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
    type Range = (f64, f64, Vec<(f64, f64)>);
    let owned: Vec<(Source, Vec<Range>)> = if report.compared {
        pieces
            .iter()
            .map(|p| {
                let ranges = if p.plans.len() == p.ranges.len() {
                    p.plans
                        .iter()
                        .map(|r| {
                            let copies = r
                                .segments
                                .iter()
                                .filter(|s| s.kind == crate::SegmentKind::Copy)
                                .map(|s| (s.start, s.end))
                                .collect();
                            (r.t_in, r.t_out, copies)
                        })
                        .collect()
                } else {
                    p.ranges.iter().map(|&(a, b)| (a, b, Vec::new())).collect()
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
                for (a, b, copies) in ranges {
                    let mut gone = false;
                    // The grid is only read where the pictures are compared.
                    let grids = compare == Compare::Pictures;
                    crate::preview::pictures_in(src, *a, *b, |t, frame| {
                        let seen = Seen {
                            hash: picture_hash(frame),
                            grid: if grids { Grid::of(frame) } else { Grid::empty() },
                            damaged: damaged(frame),
                            copied: copies.iter().any(|&(s, e)| t >= s - 1e-4 && t < e - 1e-4),
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
        let mut on_picture = |frame: &ff::frame::Video, report: &mut Report| {
            let at = frame.pts().map(|p| p as f64 * v_tb).unwrap_or(0.0);
            if let Some(p) = frame.pts() {
                first_pts = Some(first_pts.map_or(p, |f: i64| f.min(p)));
                last_pts = Some(last_pts.map_or(p, |l: i64| l.max(p)));
            }
            report.produced += 1;
            if damaged(frame) {
                report.damaged += 1;
            }
            if report.compared {
                if let Ok(seen) = rx.recv() {
                    report.expected += 1;
                    if seen.damaged {
                        report.damaged_in_source += 1;
                    }
                    let same = seen.hash == picture_hash(frame);
                    if seen.copied {
                        if let Some(n) = report.planned_copy.as_mut() {
                            *n += 1;
                            if !same {
                                report.copy_misses += 1;
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
                                if db < LIKE_DB {
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

        for (stream, packet) in ictx.read_packets() {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let i = stream.index();
            if Some(i) == video {
                let Some(dec) = decoder.as_mut() else { continue };
                if dec.send_packet(&packet).is_err() {
                    report.damaged += 1;
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
        // not have. Counted, which also lets the source side finish.
        if report.compared && !stop.load(Ordering::Relaxed) {
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
        report.sounds = sounds
            .iter()
            .map(|s| Sound { index: s.0, seconds: s.3, measured: s.3 > 0.0 })
            .collect();
        source_side.join().map_err(|_| anyhow!("the source side of the check stopped"))??;
        Ok(())
    })?;

    if stop.load(Ordering::Relaxed) {
        return Err(anyhow!("stopped"));
    }
    if let Some(t) = told.as_mut() {
        t(1.0);
    }
    Ok(report)
}

/// The output's frame rate, for the length of its last picture.
fn frame_rate(ictx: &crate::input::Demux, video: Option<usize>, fallback: f64) -> f64 {
    let r = video
        .and_then(|i| ictx.stream(i))
        .map(|s| f64::from(s.avg_frame_rate()))
        .filter(|r| r.is_finite() && *r > 0.0);
    r.unwrap_or(if fallback > 0.0 { fallback } else { 30.0 })
}
