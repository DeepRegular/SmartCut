//! Finding the stretches where the picture is flat black or flat white.
//!
//! A junction is usually laid over one: a programme fades to black before the
//! commercials, a station fades out of them, and a flash to white is what a
//! title sequence cuts on. Unlike the silences those stretches say nothing
//! about *whether* a break is there -- a fade to black happens inside a
//! programme too -- so nothing here is ranked or grouped. What comes out is
//! the stretches themselves, for a person to look at.
//!
//! **Every picture is decoded, not only the intra ones.** Every other video
//! pass here reads entry pictures alone, which on broadcast material is one
//! every half second; the stretches being looked for are shorter than that.
//! Measured on a 30-minute 1080i MPEG-2 recording off BS, this pass finds
//! four black stretches and three white ones, and five of the seven run four
//! pictures or fewer -- 0.07 s and 0.13 s. Read at the entry pictures they
//! would have fallen between two samples and been missed altogether.
//!
//! What that costs is a full decode, and what that comes to is whatever the
//! recording can be read at. On a local disc the pass ran at 103 times real
//! time -- 50 seconds of 1920x1080 MPEG-2 in half a second on sixteen cores --
//! and the same pass over the same material on a NAS share ran at 24, at which
//! point it is the network being measured and not the decoder. So half an hour
//! of broadcast is between twenty seconds and a minute and a quarter, which is
//! the same order as the logo's two passes over the entry pictures and is why
//! this is asked for rather than run with them.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;
use crate::input::ReadPackets;

use crate::Source;

/// Which way the picture is flat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shade {
    Black,
    White,
}

impl Shade {
    pub fn as_str(self) -> &'static str {
        match self {
            Shade::Black => "black",
            Shade::White => "white",
        }
    }
}

/// One stretch of flat picture.
#[derive(Debug, Clone, Copy)]
pub struct Run {
    pub shade: Shade,
    pub start: f64,
    /// Where the picture comes back: the time of the first picture that is no
    /// longer flat, rather than the last flat one.
    ///
    /// So the two ends of a run are both places a cut belongs -- the end of
    /// what came before, and the start of what comes after -- and neither of
    /// them is a picture the viewer would miss.
    pub end: f64,
    /// The last picture that is still flat, which is the one before
    /// [`Run::end`].
    ///
    /// Both are reported because the two answer different questions and the
    /// reference tool answers the other one. Cut at `end` and the flat
    /// stretch is gone exactly; stand on `last` and the picture on screen is
    /// the final frame of black. A reading held up against another tool's is
    /// otherwise a frame out at every stretch, which is what was reported.
    pub last: f64,
    /// How many pictures the run holds. Counted, not worked out from the
    /// length: a broadcast recording does not space its pictures evenly, and
    /// a stretch that repeats a field runs half a picture longer than
    /// `end - start` over `fps` says it does.
    pub pictures: usize,
}

impl Run {
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

pub struct BlankOptions {
    /// Luma at or below which a pixel counts as black, as a fraction of the
    /// way from the recording's black up to its white -- see [`Luma::span`].
    /// Nought is black itself, 16 in an 8-bit broadcast.
    ///
    /// Measured from black, and not from nought, because nought is a level a
    /// broadcast never uses. Taken as a fraction of 0..255 the bottom six
    /// percent of the scale was under 16 and found nothing in a recording
    /// that is black at 16 -- which was reported: 6% found no black at all
    /// and 7%, one code above black, found stretches. This is also what
    /// ffmpeg's `blackdetect` means by its own `pix_th`.
    ///
    /// Not "black itself" either, which is where nothing in a recording
    /// actually stays: noise, dither and the tail of a fade all sit above it.
    /// Measured across 12 to 40 out of 255 on the recording in the module
    /// note, the *number* of stretches found does not change -- the same four
    /// either way. What changes is only how much of a fade is called black:
    /// the one long stretch began a quarter of a second earlier at 40 than at
    /// 12, while the instant the picture came back was the same picture at
    /// every threshold.
    ///
    /// So one end of a fade is soft and the other is exact, which is why both
    /// ends are reported and neither is called the junction. 0.04 is 24 out
    /// of 255, the nearest whole percent under the 25 that 0.10 of 0..255
    /// came to before the level was measured from black.
    pub black_level: f64,
    /// ...and the luma at or above which it counts as white, on the same
    /// span. Higher than the black level is low, because a white frame is a
    /// deliberate flash rather than the end of a fade and is written at the
    /// top of the scale: 0.99 is 232 of the 235 that white is.
    pub white_level: f64,
    /// How much of the picture has to be that dark, or that bright, 0..1. A
    /// channel bug or a scrap of burnt-in text is a percent or two of the
    /// frame and must not keep a black frame from being one.
    ///
    /// **Raising it does not always find fewer stretches.** It finds fewer
    /// black pictures -- that much only ever goes down -- but pictures that
    /// stop counting in the middle of a long stretch cut it in two, and both
    /// halves may still be over the minimum length. Credits on black are the
    /// usual case: while a card is up its text covers two or three percent of
    /// the frame, so on a two-hour film off a movie channel a 51-second
    /// opening at 95% came apart at 96% into 10 and 26 seconds, with the
    /// seconds of a card between them, and the count went up by one.
    /// Reported from the other side as 12 stretches at 94% and 23 at 97%.
    pub coverage: f64,
    /// A border to leave out of the judgement, as a fraction of each side.
    ///
    /// Broadcast pictures carry a line or two of rubbish down the edges --
    /// half a macroblock of grey, the last column of an upscale -- and on a
    /// white frame that is enough to put the coverage under any useful
    /// threshold. It costs nothing on black, where the edge is dark anyway.
    pub inset: f64,
    /// Shortest run worth reporting, in seconds. Zero to say nothing about
    /// the length in seconds.
    pub min_seconds: f64,
    /// ...and in pictures, which is the other way somebody says the same
    /// thing. Both are applied, so a run has to satisfy whichever of them
    /// has been set.
    pub min_pictures: usize,
    /// Whether a stretch of black is wanted, and whether a stretch of white
    /// is.
    ///
    /// Both, out of the box. They are told apart here because they are not
    /// equally useful to everybody: black is where a junction is laid on
    /// broadcast, while a flash to white belongs to whatever was being
    /// watched -- a title sequence cuts on one, and so does a camera flash in
    /// a news item, which on some material is dozens of stretches nobody
    /// asked about. Turning one off is not a filter over the answer: the
    /// pictures are never called that shade in the first place, so what is
    /// written down is the question that was asked.
    pub black: bool,
    pub white: bool,
    /// How many cores the decode may have. Zero, the default, is all of them.
    pub threads: usize,
}

impl Default for BlankOptions {
    fn default() -> Self {
        Self {
            black_level: 0.04,
            white_level: 0.99,
            coverage: 0.98,
            inset: 0.02,
            min_seconds: 0.0,
            min_pictures: 2,
            black: true,
            white: true,
            threads: 0,
        }
    }
}

/// How many samples a picture is judged on, across and down.
///
/// The decode is the cost of this pass, not the counting, so the grid is
/// about being able to say "98% of the picture" with a straight face rather
/// than about speed: 128 by 72 is nine thousand pixels, and a percent of the
/// picture is ninety of them.
const GRID_X: usize = 128;
const GRID_Y: usize = 72;

/// What one picture came to.
#[derive(Debug, Clone, Copy)]
struct Look {
    time: f64,
    shade: Option<Shade>,
}

/// Where the luma plane is, and how wide a sample of it is.
///
/// Read from the format rather than assumed. 4K off a recorder is 10-bit
/// HEVC, and reading its luma as bytes finds half a sample in every one:
/// noise, and never black. The colour planes are not touched at all -- a
/// picture this pass calls black may be any colour, and a flat blue frame is
/// not what anybody is looking for -- so a semi-planar format needs no
/// special case either, its luma being plane 0 like everything else's.
struct Luma {
    /// Bits per sample, from the format's own descriptor.
    depth: u32,
    big_endian: bool,
}

impl Luma {
    fn of(format: ff::format::Pixel) -> Result<Self> {
        // SAFETY: `av_pix_fmt_desc_get` takes a format and returns a pointer
        // into libavutil's own static table, or null for a format it does not
        // know. Nothing is owned and nothing is freed.
        let desc = unsafe { ff::ffi::av_pix_fmt_desc_get(format.into()) };
        if desc.is_null() {
            return Err(anyhow!("unknown pixel format {format:?}"));
        }
        let desc = unsafe { &*desc };
        if desc.flags & (ff::ffi::AV_PIX_FMT_FLAG_RGB as u64) != 0 {
            // No luma plane to read. Nothing that decodes a recording lands
            // here; a still image fed in by hand could.
            return Err(anyhow!("{format:?} has no luma plane"));
        }
        if desc.flags & (ff::ffi::AV_PIX_FMT_FLAG_PAL as u64) != 0 {
            return Err(anyhow!("{format:?} is palettised"));
        }
        let depth = desc.comp[0].depth as u32;
        if depth == 0 || depth > 16 {
            return Err(anyhow!("{format:?} carries {depth}-bit luma"));
        }
        Ok(Self {
            depth,
            big_endian: desc.flags & (ff::ffi::AV_PIX_FMT_FLAG_BE as u64) != 0,
        })
    }

    /// Where black and white are at this depth: the two ends a level is a
    /// fraction of the way between.
    ///
    /// **Shifted, not scaled to the largest sample.** The same picture at two
    /// depths is the same code values shifted up -- white is 235 at 8 bits
    /// and 940 at 10, which is 235 times four -- while the largest sample
    /// goes 255 to 1023, which is not 255 times four. Taking the fraction of
    /// that would make every level a shade stricter the deeper the recording
    /// is written, and a 10-bit flash to white in a 4K recording off a
    /// recorder was missed that way.
    ///
    /// **Studio levels whatever range the picture says it is in**, the ones a
    /// fade is written with (`transition::Shade::yuv`). A full-range picture's
    /// black is under the bottom of this span and its white over the top, so
    /// both are still found, and a level means the same luma on a phone's
    /// recording as on a broadcast one. Measured from nought and full scale
    /// there instead, the defaults came to 10 and 252 on full-range material,
    /// where 0.8.5 had looked for 25 and 234 -- a phone's noisy fade to black
    /// and its JPEG-soft flash to white were both missed.
    fn span(&self) -> (f64, f64) {
        if self.depth < 8 {
            return (0.0, ((1u32 << self.depth) - 1) as f64);
        }
        let black = crate::transition::Shade::Black.yuv(self.depth).0;
        let white = crate::transition::Shade::White.yuv(self.depth).0;
        (black as f64, white as f64)
    }

    /// The sample a level comes to.
    ///
    /// The fraction is dropped, with a hair of slack under a whole sample: a
    /// level typed as a luma value in 環境設定 is carried as the percent it
    /// comes to, and 17 out of 16..235 comes back as 16.9999... on the way
    /// here, which is not the 17 that was typed.
    fn level(&self, fraction: f64) -> u32 {
        let (black, white) = self.span();
        (black + fraction.clamp(0.0, 1.0) * (white - black) + 1e-6) as u32
    }

    /// One sample out of the plane's bytes.
    fn at(&self, row: &[u8], x: usize) -> u32 {
        if self.depth <= 8 {
            row[x] as u32
        } else {
            let i = x * 2;
            let (a, b) = (row[i], row[i + 1]);
            if self.big_endian {
                u16::from_be_bytes([a, b]) as u32
            } else {
                u16::from_le_bytes([a, b]) as u32
            }
        }
    }
}

/// Is this picture flat, and which way?
fn look_at(frame: &ff::frame::Video, luma: &Luma, opts: &BlankOptions) -> Option<Shade> {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let inset = opts.inset.clamp(0.0, 0.4);
    let (x0, x1) = ((w as f64 * inset) as usize, w - (w as f64 * inset) as usize);
    let (y0, y1) = ((h as f64 * inset) as usize, h - (h as f64 * inset) as usize);
    let (iw, ih) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    if iw == 0 || ih == 0 {
        return None;
    }
    let dark = luma.level(opts.black_level);
    let bright = luma.level(opts.white_level);
    let stride = frame.stride(0);
    let data = frame.data(0);
    let cols = GRID_X.min(iw);
    let rows = GRID_Y.min(ih);
    let mut seen = 0usize;
    let mut black = 0usize;
    let mut white = 0usize;
    for j in 0..rows {
        let y = y0 + j * ih / rows;
        let from = y * stride;
        let Some(row) = data.get(from..from + stride) else {
            // A plane shorter than its own stride says the frame was not
            // decoded whole. Judging it would be judging whatever is in the
            // buffer.
            return None;
        };
        for i in 0..cols {
            let x = x0 + i * iw / cols;
            let v = luma.at(row, x);
            seen += 1;
            if opts.black && v <= dark {
                black += 1;
            } else if opts.white && v >= bright {
                white += 1;
            }
        }
    }
    if seen == 0 {
        return None;
    }
    let need = opts.coverage.clamp(0.0, 1.0) * seen as f64;
    if opts.black && (black as f64) >= need {
        Some(Shade::Black)
    } else if opts.white && (white as f64) >= need {
        Some(Shade::White)
    } else {
        None
    }
}

/// Turn what each picture came to into the runs worth reporting.
///
/// `tail` is where the last picture ends, so that a recording finishing on
/// black still says where that black stopped.
fn runs_from(looks: &[Look], tail: f64, opts: &BlankOptions) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    // The shade, where the run began, how many pictures it holds, and the
    // last of them.
    let mut open: Option<(Shade, f64, usize, f64)> = None;
    let close = |out: &mut Vec<Run>, open: Option<(Shade, f64, usize, f64)>, end: f64| {
        if let Some((shade, start, pictures, last)) = open {
            out.push(Run {
                shade,
                start,
                end,
                last,
                pictures,
            });
        }
    };
    for look in looks {
        match (open, look.shade) {
            (Some((shade, start, n, _)), Some(now)) if shade == now => {
                open = Some((shade, start, n + 1, look.time));
            }
            (held, now) => {
                close(&mut out, held, look.time);
                open = now.map(|shade| (shade, look.time, 1, look.time));
            }
        }
    }
    close(&mut out, open, tail);
    out.retain(|r| r.pictures >= opts.min_pictures.max(1) && r.duration() >= opts.min_seconds);
    out
}

/// What the decode has seen so far.
struct Walk {
    /// Where the luma is, worked out from the first picture that arrives. A
    /// recording does not change format halfway through, and one that somehow
    /// did would be read by the first format's rule -- which is the format
    /// its own pictures were judged by.
    luma: Option<Luma>,
    looks: Vec<Look>,
    /// The last time taken, so that a picture out of order can be passed over.
    last: f64,
    /// The gap before it; see where this is set up.
    gap: f64,
}

impl Walk {
    fn take(&mut self, frame: &ff::frame::Video, src: &Source, opts: &BlankOptions) -> Result<()> {
        let Some(pts) = frame.pts() else { return Ok(()) };
        let t = pts as f64 * src.video.time_base - src.start_time;
        // Out of order, which a recording off air manages at a discontinuity.
        // Taken as it stands it would put a run's two ends the wrong way
        // round.
        if t <= self.last {
            return Ok(());
        }
        if self.last.is_finite() {
            self.gap = t - self.last;
        }
        self.last = t;
        if self.luma.is_none() {
            self.luma = Some(Luma::of(frame.format())?);
        }
        let luma = self.luma.as_ref().expect("just set");
        self.looks.push(Look {
            time: t,
            shade: look_at(frame, luma, opts),
        });
        Ok(())
    }
}

/// Walk the pictures and note every flat stretch.
pub fn find_runs(src: &Source, opts: &BlankOptions) -> Result<Vec<Run>> {
    find_runs_with(src, opts, None)
}

/// As [`find_runs`], reporting how far through the recording it has read.
pub fn find_runs_with(
    src: &Source,
    opts: &BlankOptions,
    mut progress: Option<Box<dyn FnMut(f64) + Send>>,
) -> Result<Vec<Run>> {
    crate::init()?;
    let mut ictx = crate::input::demux(&src.input.url)?;
    let idx = src.video.stream_index;
    // Only the pictures are read. See [`crate::input::keep_only`].
    crate::input::keep_only(&mut ictx, &[idx]);
    let params = ictx
        .stream(idx)
        .ok_or_else(|| anyhow!("video stream vanished"))?
        .parameters();
    let mut decoder = crate::video_decoder_with(params, opts.threads)?;

    let mut walk = Walk {
        luma: None,
        looks: Vec::new(),
        last: f64::NEG_INFINITY,
        // Uneven, so it is measured rather than taken from the frame rate: a
        // broadcast recording does not space its pictures evenly. Only the
        // last picture's end is wanted from it, and the gap before it is the
        // best guess at that.
        gap: 1.0 / src.video.frame_rate.max(1.0),
    };
    let mut frame = ff::frame::Video::empty();
    let mut told = -1.0;

    for (stream, packet) in ictx.read_packets() {
        if stream.index() != idx {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        while decoder.receive_frame(&mut frame).is_ok() {
            walk.take(&frame, src, opts)?;
        }
        if let Some(f) = progress.as_mut() {
            let done = (walk.last.max(0.0) / src.duration.max(1e-9)).clamp(0.0, 1.0);
            if done - told >= 0.02 {
                told = done;
                f(done);
            }
        }
    }
    // The pictures a decoder is still holding are the end of the recording,
    // which is exactly where a fade to black tends to be.
    if decoder.send_eof().is_ok() {
        while decoder.receive_frame(&mut frame).is_ok() {
            walk.take(&frame, src, opts)?;
        }
    }
    if let Some(f) = progress.as_mut() {
        f(1.0);
    }
    let tail = walk.last + walk.gap;
    Ok(runs_from(&walk.looks, tail, opts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn looks(spec: &[(f64, Option<Shade>)]) -> Vec<Look> {
        spec.iter()
            .map(|&(time, shade)| Look { time, shade })
            .collect()
    }

    const B: Option<Shade> = Some(Shade::Black);
    const W: Option<Shade> = Some(Shade::White);
    const N: Option<Shade> = None;

    #[test]
    fn a_run_ends_where_the_picture_comes_back() {
        let opts = BlankOptions {
            min_pictures: 1,
            ..Default::default()
        };
        let runs = runs_from(
            &looks(&[(0.0, N), (0.1, B), (0.2, B), (0.3, N), (0.4, N)]),
            0.5,
            &opts,
        );
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].shade, Shade::Black);
        assert_eq!(runs[0].start, 0.1);
        // 0.3 -- the first picture that is not black -- and not 0.2.
        assert_eq!(runs[0].end, 0.3);
        // ...and 0.2 is the last one that is, which the other tool reports
        // and 環境設定 can ask for.
        assert_eq!(runs[0].last, 0.2);
        assert_eq!(runs[0].pictures, 2);
    }

    #[test]
    fn black_next_to_white_is_two_runs() {
        let opts = BlankOptions {
            min_pictures: 1,
            ..Default::default()
        };
        let runs = runs_from(&looks(&[(0.0, B), (0.1, W), (0.2, N)]), 0.3, &opts);
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0].shade, runs[0].end), (Shade::Black, 0.1));
        assert_eq!((runs[1].shade, runs[1].end), (Shade::White, 0.2));
    }

    #[test]
    fn a_recording_that_ends_black_ends_its_run() {
        let opts = BlankOptions {
            min_pictures: 1,
            ..Default::default()
        };
        let runs = runs_from(&looks(&[(0.0, N), (0.1, B), (0.2, B)]), 0.3, &opts);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].end, 0.3);
        assert_eq!(runs[0].last, 0.2);
    }

    #[test]
    fn both_minimums_are_applied() {
        let spec = [(0.0, N), (0.1, B), (0.2, B), (0.3, N)];
        let by_pictures = runs_from(
            &looks(&spec),
            0.4,
            &BlankOptions {
                min_pictures: 3,
                ..Default::default()
            },
        );
        assert!(by_pictures.is_empty(), "two pictures is not three");
        let by_seconds = runs_from(
            &looks(&spec),
            0.4,
            &BlankOptions {
                min_pictures: 1,
                min_seconds: 0.5,
                ..Default::default()
            },
        );
        assert!(by_seconds.is_empty(), "0.2 s is not half a second");
        let both = runs_from(
            &looks(&spec),
            0.4,
            &BlankOptions {
                min_pictures: 2,
                min_seconds: 0.15,
                ..Default::default()
            },
        );
        assert_eq!(both.len(), 1);
    }

    /// A level is a fraction of the same span the samples are shifted on,
    /// so the same picture reads the same whether its luma is 8-bit or
    /// 10-bit.
    #[test]
    fn depth_is_read_from_the_format() {
        let eight = Luma::of(ff::format::Pixel::YUV420P).unwrap();
        assert_eq!((eight.depth, eight.span()), (8, (16.0, 235.0)));
        let ten = Luma::of(ff::format::Pixel::YUV420P10LE).unwrap();
        assert_eq!((ten.depth, ten.span()), (10, (64.0, 940.0)));
        assert!(!ten.big_endian);
        assert_eq!(ten.at(&[0x40, 0x00], 0), 64);
        assert_eq!(eight.at(&[16], 0), 16);
    }

    /// A picture of one flat shade, for [`look_at`] to judge.
    fn flat(value: u8) -> ff::frame::Video {
        let mut frame = ff::frame::Video::new(ff::format::Pixel::YUV420P, 64, 64);
        frame.data_mut(0).fill(value);
        frame
    }

    /// A shade that has been turned off is not looked for, rather than found
    /// and dropped afterwards: what goes on disc is the question that was
    /// asked, so a detection for black alone never says the word white.
    #[test]
    fn a_shade_turned_off_is_not_reported() {
        let luma = Luma::of(ff::format::Pixel::YUV420P).unwrap();
        let both = BlankOptions::default();
        assert_eq!(look_at(&flat(16), &luma, &both), Some(Shade::Black));
        assert_eq!(look_at(&flat(235), &luma, &both), Some(Shade::White));

        let dark_only = BlankOptions {
            white: false,
            ..Default::default()
        };
        assert_eq!(look_at(&flat(16), &luma, &dark_only), Some(Shade::Black));
        assert_eq!(look_at(&flat(235), &luma, &dark_only), None);

        let pale_only = BlankOptions {
            black: false,
            ..Default::default()
        };
        assert_eq!(look_at(&flat(16), &luma, &pale_only), None);
        assert_eq!(look_at(&flat(235), &luma, &pale_only), Some(Shade::White));
    }

    /// Broadcast white is 235 at 8 bits and 940 at 10, and the default level
    /// has to call both of them white. Taken against the largest sample the
    /// depth holds it called the 10-bit one grey, and a flash to white in a
    /// 4K recording off a recorder was never reported.
    #[test]
    fn white_reads_the_same_at_either_depth() {
        let level = BlankOptions::default().white_level;
        let eight = Luma::of(ff::format::Pixel::YUV420P).unwrap();
        let ten = Luma::of(ff::format::Pixel::YUV420P10LE).unwrap();
        assert!(235 >= eight.level(level));
        assert!(940 >= ten.level(level));
        // ...and black, which is 16 and 64, stays under the other end.
        let dark = BlankOptions::default().black_level;
        assert!(16 <= eight.level(dark));
        assert!(64 <= ten.level(dark));
    }

    /// A luma value typed in 環境設定 is carried as the percent it comes to,
    /// `(v - 16) * 100 / 219`, and has to come back as that value -- at
    /// either depth -- and not as the one under it.
    #[test]
    fn a_typed_value_comes_back_whole() {
        let eight = Luma::of(ff::format::Pixel::YUV420P).unwrap();
        let ten = Luma::of(ff::format::Pixel::YUV420P10LE).unwrap();
        for v in 16..=235u32 {
            let fraction = ((v as f64 - 16.0) * 100.0 / 219.0) / 100.0;
            assert_eq!(eight.level(fraction), v, "{v}");
            assert_eq!(ten.level(fraction), v << 2, "{v}");
        }
    }

    /// Nought is black itself, so every level on the scale finds a picture
    /// written at broadcast black. Measured against 0..255 the bottom six
    /// percent was under 16, and a black frame was not black at any of them.
    #[test]
    fn the_bottom_of_the_scale_is_black() {
        let luma = Luma::of(ff::format::Pixel::YUV420P).unwrap();
        for percent in 0..=10 {
            let opts = BlankOptions {
                black_level: percent as f64 / 100.0,
                ..Default::default()
            };
            assert_eq!(look_at(&flat(16), &luma, &opts), Some(Shade::Black), "{percent}%");
        }
        // One step above black is not black at nought...
        let none = BlankOptions {
            black_level: 0.0,
            ..Default::default()
        };
        assert_eq!(look_at(&flat(17), &luma, &none), None);
        // ...and a full-range picture is judged on the same span: its black,
        // nought, is under the bottom of it, and its white over the top.
        let full = Luma::of(ff::format::Pixel::YUVJ420P).unwrap();
        assert_eq!(full.span(), (16.0, 235.0));
        assert_eq!(look_at(&flat(0), &full, &none), Some(Shade::Black));
        assert_eq!(look_at(&flat(255), &full, &none), Some(Shade::White));
    }
}
