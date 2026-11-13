//! What a seam between two clips looks like, for the window that sets one.
//!
//! The transition settings used to be six rows on a settings screen with
//! nothing to look at. What `ディゾルブ 1.4 秒 Sine イン-アウト` does to two
//! particular recordings is not something anybody can read off those rows,
//! so this decodes it: the seconds either side of the join, composited the
//! way the cut will composite them, as pictures a window can show and play.
//!
//! **None of the arithmetic is repeated here.** Every sample this produces
//! goes through [`crate::blend`], and how far through the crossing each
//! instant is comes from [`crate::transition`] -- the same two modules the
//! cutter calls, with the same numbers. What is written twice is the
//! *schedule*: which instant of the output shows what. [`crate::cut`] builds
//! that as plan segments around a whole edit; this builds it around one join,
//! because a preview has no edit -- it has two recordings and a setting. The
//! tests at the foot of this file are what hold the two answers together.
//!
//! ## The shape everything is brought to
//!
//! Both clips are scaled to one size in `yuv420p` before anything is blended,
//! which is what [`crate::blend`] requires and what the cutter does for the
//! same reason -- there, to the master clip's shape. Here the size is the
//! preview's own, and the range is the studio range the cutter works in:
//! [`crate::transition::Shade`] gives studio black, and a composite done in
//! the full-range `yuvj420p` a JPEG wants would put that at the wrong level.
//! The conversion to JPEG happens after the blend, never before.

use anyhow::{anyhow, Result};
use ffmpeg_next as ff;
use crate::input::ReadPackets;

use crate::blend::Laid;
use crate::transition::{Crossing, Easing, Shade, Transition};
use crate::Source;

/// How far either side of the crossing the preview reaches, in seconds.
///
/// Enough to see what the crossing comes out of and what it goes into, and
/// not so much that the window is mostly programme. A transition longer than
/// this pushes the lead out with it -- see [`Seam::window`] -- so a 30 second
/// dissolve is never previewed through a keyhole.
pub const LEAD: f64 = 3.0;

/// The format both clips are brought to before they are blended.
const SHAPE: ff::format::Pixel = ff::format::Pixel::YUV420P;

/// The two clips at one join, and what happens between them.
///
/// The times are each clip's own, and they are the bounds of the *kept*
/// material at this seam: where the clip before the join stops, and where the
/// clip after it starts. The cut's own ranges are what supply them -- the
/// last range of one reel and the first of the next -- which is why they are
/// passed in rather than read off the recordings. A recording is an hour
/// long; what is being joined may be four minutes of it.
pub struct Seam<'a> {
    pub before: &'a Source,
    /// Where the clip before the join begins, which is what caps the lead-in
    /// and half of what caps the crossing.
    pub before_in: f64,
    /// And where it stops. The crossing is worked backwards from here.
    pub before_out: f64,
    pub after: &'a Source,
    /// Where the clip after the join begins. The crossing is worked forwards
    /// from here.
    pub after_in: f64,
    pub after_out: f64,
    pub transition: Transition,
    /// The recording whose shape the joined file is written in -- the
    /// master -- where that is not the clip before the join. Both clips are
    /// composited at its shape, as the cut writes every reel into it (see
    /// [`crate::conform`]). `None` takes the clip before's.
    pub shaped_like: Option<&'a Source>,
}

impl Seam<'_> {
    /// The size the preview's pictures are composited at: the master's
    /// shape, at most `width` wide. See [`shape_for`].
    pub fn shape(&self, width: u32) -> (u32, u32) {
        shape_for(self.master(), width)
    }

    /// The recording the joined file is written in the shape of.
    fn master(&self) -> &Source {
        self.shaped_like.unwrap_or(self.before)
    }

    /// How much of each clip the crossing really takes.
    ///
    /// [`Transition::takes`] says what it asks for; this is what the material
    /// allows. Each end is held to half the range it takes from -- a
    /// transition longer than the clip it is joining is a transition with
    /// nothing left to join.
    ///
    /// An overlapping crossing cannot be lopsided: the two clips are shown
    /// *together*, so what one end spends is what the other gives up, and it
    /// runs for whichever of the two allows less. A fade is two separate
    /// halves written in place, so each of those is capped on its own. The
    /// cutter answers both the same way; see `ranges_with_transitions` in
    /// [`crate::cut`].
    pub fn takes(&self) -> (f64, f64) {
        let (want_before, want_after) = self.transition.takes();
        let room_before = ((self.before_out - self.before_in) / 2.0).max(0.0);
        let room_after = ((self.after_out - self.after_in) / 2.0).max(0.0);
        let (before, after) = (want_before.min(room_before), want_after.min(room_after));
        if self.transition.kind.overlaps() {
            let both = before.min(after);
            (both, both)
        } else {
            (before, after)
        }
    }

    /// The previewed stretch: what is on screen at each instant of it.
    ///
    /// `lead` is what is wanted either side of the crossing and not what is
    /// given: a clip with two seconds left before the join has two seconds of
    /// lead, and the window says so by being shorter.
    pub fn window(&self, lead: f64) -> Window {
        let (take_before, take_after) = self.takes();
        let overlaps = self.transition.kind.overlaps();
        // The same lead whatever the crossing's length. A thirty second
        // dissolve previewed with three seconds either side is a preview that
        // is mostly dissolve, which is right: the dissolve is the thing being
        // looked at, and the lead is only there to say what it comes out of
        // and what it goes into.
        let lead_before = lead.min(self.before_out - take_before - self.before_in).max(0.0);
        let lead_after = lead.min(self.after_out - self.after_in - take_after).max(0.0);
        // Where the crossing starts and stops on the window's own clock. For
        // a fade the two halves are one crossing: the clip before goes down
        // into the colour and the clip after comes up out of it, and an image
        // laid over it is one image over the pair.
        let at = lead_before;
        // An overlapping crossing runs once over both clips; a fade runs
        // twice, once down into the colour and once back up out of it.
        let ends = if overlaps {
            at + take_before
        } else {
            at + take_before + take_after
        };
        Window {
            seconds: ends + lead_after,
            at,
            ends,
            before_from: self.before_out - take_before - lead_before,
            after_in: self.after_in,
            // When the clip after the join first appears. During an
            // overlapping crossing it is on screen from the first instant of
            // it; a fade does not reach it until the colour has been arrived
            // at, and an ordinary cut not until the cut itself.
            after_at: if overlaps { at } else { at + take_before },
            take_before,
            take_after,
            overlaps,
            kind: self.transition.kind,
            easing: self.transition.easing,
        }
    }
}

/// The previewed stretch, and what it shows at each instant.
#[derive(Debug, Clone)]
pub struct Window {
    /// How long the whole preview runs.
    pub seconds: f64,
    /// Where the crossing begins on the window's clock, and where it ends.
    /// Equal where there is no crossing, which is where the join is a cut.
    pub at: f64,
    pub ends: f64,
    /// What the clip before the join is showing at window time 0.
    before_from: f64,
    after_in: f64,
    after_at: f64,
    take_before: f64,
    take_after: f64,
    overlaps: bool,
    kind: Crossing,
    easing: Easing,
}

/// What the preview shows at one instant.
#[derive(Debug, Clone, PartialEq)]
pub struct Look {
    /// Where in the clip before the join, while it is still on screen.
    pub near: Option<f64>,
    /// Where in the clip after it, once it is.
    pub far: Option<f64>,
    pub how: How,
    /// How solid a still laid over the crossing is here. Nought outside one.
    pub overlay: f64,
}

/// What is done with the picture, or pictures, at one instant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum How {
    /// One clip, as it was recorded.
    Plain,
    /// One clip, on its way into a colour or out of one. `mix` is how much of
    /// the colour is there.
    Tint { shade: Shade, mix: f64 },
    /// Both clips at once. `alpha` is how far the one after the join has come.
    Cross { kind: Crossing, alpha: f64 },
}

/// The stretch of each clip's sound the preview plays, and where one gives
/// way to the other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sound {
    /// On the clip before the join's own clock.
    pub before: (f64, f64),
    /// On the clip after it.
    pub after: (f64, f64),
    /// Where one gives way to the other, on the preview's own clock.
    pub at: f64,
}

impl Window {
    /// Which sound plays when.
    ///
    /// **Nothing is mixed.** A crossing shows two pictures at once; the sound
    /// of a join is the one clip and then the other, and that is what the
    /// cutter writes. The clip before the join keeps its own end -- its
    /// pictures are consumed by the crossing, its sound plays through it --
    /// and the clip after starts where its own range starts, which an
    /// overlapping crossing has already spent the first seconds of. See
    /// `ranges_with_transitions` in [`crate::cut`], which is where that rule
    /// is decided for the output this stands for.
    ///
    /// So the handover is not always the end of the crossing. Through a fade
    /// it is the middle of it -- the instant the colour is arrived at, which
    /// is where one clip's material stops and the other's begins.
    pub fn sound(&self) -> Sound {
        let at = if self.overlaps { self.ends } else { self.after_at };
        Sound {
            before: (self.before_from, self.before_from + at),
            after: (
                self.after_in + (at - self.after_at),
                self.after_in + (self.seconds - self.after_at),
            ),
            at,
        }
    }

    /// What is on screen `t` seconds into the preview.
    pub fn look(&self, t: f64) -> Look {
        let t = t.clamp(0.0, self.seconds);
        // The clip before the join is on screen until its own material runs
        // out, which is the end of the crossing's first half -- or the cut
        // itself, where there is no crossing.
        let near = (t < self.at + self.take_before - 1e-9).then_some(self.before_from + t);
        let far = (t >= self.after_at - 1e-9).then_some(self.after_in + (t - self.after_at));
        // Before the crossing and after it, one clip is simply playing.
        if t < self.at - 1e-9 || t >= self.ends - 1e-9 {
            return Look {
                near: if t < self.at - 1e-9 { near } else { None },
                far: if t < self.at - 1e-9 { None } else { far },
                how: How::Plain,
                overlay: 0.0,
            };
        }
        let whole = (self.ends - self.at).max(1e-9);
        let across = ((t - self.at) / whole).clamp(0.0, 1.0);
        let how = match self.kind {
            Crossing::Fade(shade) if !self.overlaps => {
                // Two halves: the clip before goes down into the colour, the
                // clip after comes up out of it.
                let going_in = t < self.at + self.take_before - 1e-9;
                let (from, of) = if going_in {
                    (t - self.at, self.take_before)
                } else {
                    (t - self.at - self.take_before, self.take_after)
                };
                let through = (from / of.max(1e-9)).clamp(0.0, 1.0);
                let a = self.easing.at(through);
                How::Tint {
                    shade,
                    mix: if going_in { a } else { 1.0 - a },
                }
            }
            kind => How::Cross {
                kind,
                alpha: self.easing.at(across),
            },
        };
        // Inside a fade only one of the two clips is on screen at a time; the
        // other has not started or has finished.
        let (near, far) = match how {
            How::Tint { .. } => {
                let going_in = t < self.at + self.take_before - 1e-9;
                if going_in {
                    (near, None)
                } else {
                    (None, far)
                }
            }
            _ => (near, far),
        };
        Look {
            near,
            far,
            how,
            overlay: crate::transition::showing(across),
        }
    }
}

/// The size the preview's pictures are composited at.
///
/// Square pixels, worked out from the recording whose shape the joined file
/// takes ([`Seam::shape`]): a 4:3 recording joined onto a 16:9 master is
/// written filled into 16:9, and composited at its own shape the window
/// showed a seam the file does not have. Even in both directions, because
/// `yuv420p` has half as many chroma samples.
pub fn shape_for(src: &Source, width: u32) -> (u32, u32) {
    let sar = src.video.sample_aspect_ratio.max(0.01);
    let native = (f64::from(src.video.width) * sar).round() as u32;
    let w = width.min(native.max(16)).max(16) & !1;
    // Held to a size a frame can be allocated at. A stream whose parameters
    // never said how wide it is divided by nought here, and the height came
    // out as `u32::MAX`, which the frame allocation reads as negative and
    // leaves without planes for the blend to write into.
    let h = ((f64::from(w) * f64::from(src.video.height)
        / (f64::from(src.video.width.max(1)) * sar))
        .round() as u32)
        .clamp(16, 8192)
        & !1;
    (w, h)
}

/// A picture on its way into the preview's shape.
///
/// The scaler is rebuilt whenever the decoder starts handing over a different
/// shape, which is not the paranoia it looks like: a broadcast recording can
/// change frame size mid-file where the station switched feeds, and a scaler
/// built for the first shape refuses the second. The cutter keeps one of
/// these for the same reason.
struct Rescale {
    was: (u32, u32, ff::format::Pixel, bool),
    /// The matrices it converts between; see [`crate::cut::matrices`].
    matrices: Option<(i32, i32)>,
    ctx: crate::blend::Scaler,
}

/// `master` is the recording the joined file is written in the shape of
/// ([`Seam::master`]): a reel whose samples were made with another YUV matrix
/// than the master's is converted to the master's, as the cut converts it,
/// and a standard-definition reel joined onto a high-definition master was
/// shown in other colours from the ones the file has. `None` converts none.
fn reshape(
    frame: &ff::frame::Video,
    (w, h): (u32, u32),
    master: Option<&crate::VideoInfo>,
    rescale: &mut Option<Rescale>,
) -> Result<ff::frame::Video> {
    // A palette reel's colours looked up first, as the cut looks them up
    // ([`crate::blend::palette_as_rgba`]).
    let looked_up = crate::blend::palette_as_rgba(frame);
    let frame = looked_up.as_ref().unwrap_or(frame);
    // Every picture brought to studio levels, whatever range it was in. The
    // two clips at a seam can be in different ranges -- that is one of the
    // reasons a clip is written afresh -- and a dissolve or a wipe mixes
    // their samples as they stand: one full and one studio came out of the
    // blend as neither. The output brings both to the master's range before
    // it blends; the preview brings both to one range for the same reason,
    // and studio is the one the rest of the preview reads a `yuv420p`
    // picture in.
    let full = crate::blend::full_range_frame(frame);
    let have = (frame.width(), frame.height(), frame.format(), full);
    let swap = master.and_then(|m| crate::cut::matrices(frame, m));
    // An RGB master's composite is BT.601 YUV, not the RGB the cut writes:
    // a YUV reel the cut turns into RGB through its own matrix comes into
    // this one from that matrix to BT.601, or it showed in other colours.
    let bt601 = ff::ffi::AVColorSpace::AVCOL_SPC_SMPTE170M as i32;
    let swap = match swap {
        Some((from, _)) if master.is_some_and(rgb_master) => {
            (from != bt601).then_some((from, bt601))
        }
        other => other,
    };
    if swap.is_none() && have == (w, h, SHAPE, false) {
        return Ok(frame.clone());
    }
    if rescale.as_ref().is_none_or(|r| r.was != have || r.matrices != swap) {
        let mut ctx = crate::blend::Scaler::with_flags(
            have,
            (w, h, SHAPE, false),
            // A preview is thrown away the instant after it is looked at,
            // and at playback it is one of thirty a second: the cutter's
            // bicubic buys a quality nobody sees here and costs time that
            // is the difference between playing and stuttering.
            ff::software::scaling::Flags::FAST_BILINEAR,
        )?;
        if let Some(swap) = swap {
            crate::cut::convert_matrices(&mut ctx, swap, full, false);
        }
        *rescale = Some(Rescale {
            was: have,
            matrices: swap,
            ctx,
        });
    }
    let mut out = ff::frame::Video::new(SHAPE, w, h);
    rescale
        .as_mut()
        .expect("just set")
        .ctx
        .run(frame, &mut out)?;
    out.set_color_range(ff::util::color::Range::MPEG);
    Ok(out)
}

/// Put one instant of the preview together.
///
/// `near` is the clip before the join and `far` the one after, both already
/// in the preview's shape. Either may be missing -- one clip is not on screen
/// at this instant, or has run out of pictures -- and what is done then is
/// what the cutter does: whatever is there is shown, and nothing is invented
/// for what is not.
fn compose(
    near: Option<&ff::frame::Video>,
    far: Option<&ff::frame::Video>,
    look: &Look,
    laid: Option<&Laid>,
    (w, h): (u32, u32),
) -> Result<ff::frame::Video> {
    let mut out = ff::frame::Video::new(SHAPE, w, h);
    let one = near.or(far);
    match look.how {
        How::Plain => {
            let Some(from) = one else {
                return Err(anyhow!("nothing to show at this instant"));
            };
            crate::blend::copy_into(from, &mut out)?;
        }
        How::Tint { shade, mix } => {
            let Some(from) = one else {
                return Err(anyhow!("nothing to fade at this instant"));
            };
            crate::blend::copy_into(from, &mut out)?;
            crate::blend::tint(&mut out, shade, mix)?;
        }
        How::Cross { kind, alpha } => match (near, far) {
            (Some(from), Some(theirs)) => match kind {
                Crossing::Dissolve => crate::blend::dissolve(from, theirs, &mut out, alpha)?,
                Crossing::Wipe(side) => {
                    crate::blend::wipe(from, theirs, &mut out, side, alpha, false)?
                }
                Crossing::Slide(side) => {
                    crate::blend::wipe(from, theirs, &mut out, side, alpha, true)?
                }
                // A fade has no far side and reaches this through `Tint`;
                // `None` is not a transition at all.
                _ => crate::blend::copy_into(from, &mut out)?,
            },
            (Some(from), None) | (None, Some(from)) => crate::blend::copy_into(from, &mut out)?,
            (None, None) => return Err(anyhow!("nothing to cross at this instant")),
        },
    }
    if let (Some(laid), true) = (laid, look.overlay > 0.0) {
        crate::blend::over(&mut out, laid, look.overlay)?;
    }
    Ok(out)
}

/// The still laid over the crossing, where the transition has one.
///
/// Read once per preview rather than per picture, exactly as the cutter reads
/// it once per run. An image that will not open is not a reason to show
/// nothing: the seam is still worth looking at, so the reason is handed back
/// and the caller decides.
///
/// At studio levels, which is what [`reshape`] brings every picture of the
/// preview to; the output lays it in the master's range.
///
/// **And in the master's YUV matrix**, which the composite it is laid on is
/// said to be in ([`in_the_masters_matrix`]). Read by the preview's own size
/// -- a few hundred pixels across, so BT.601 -- the image went in through
/// BT.601 and was then shown through an HD master's BT.709 with the rest of
/// the composite: a red title orange in the window and red in the output.
fn overlay_of(
    transition: &Transition,
    shape: (u32, u32),
    master: &crate::VideoInfo,
) -> Result<Option<Laid>> {
    let matrix = masters_matrix(master);
    match transition.overlay.as_deref() {
        None => Ok(None),
        Some(path) => {
            crate::blend::read_laid_in(path, shape.0, shape.1, SHAPE, false, matrix).map(Some)
        }
    }
}

/// Say which YUV matrix a composite's samples are in: the master's, since
/// [`reshape`] has brought every reel into it. The composite is a new frame
/// at the preview's size, which states none, and the JPEG it is turned into
/// reads an unstated matrix by the size it has -- a few hundred pixels
/// across, so BT.601 -- and an HD master's BT.709 samples were shown through
/// the wrong one. Stated, or read by the master's own size where it states
/// nothing, as the cut and every player read it
/// ([`crate::conform::read_matrix`]).
fn in_the_masters_matrix(picture: &mut ff::frame::Video, master: &crate::VideoInfo) {
    if let Some(space) = masters_matrix(master) {
        unsafe {
            crate::set_raw_enum(std::ptr::addr_of_mut!((*picture.as_mut_ptr()).colorspace), space);
        }
    }
}

/// The matrix [`in_the_masters_matrix`] and [`overlay_of`] put the window's
/// pictures in: [`crate::conform::read_matrix`]'s, and where that has none --
/// a master stating YCgCo or a reserved value, which no reel is converted to
/// or from -- the one its size reads, as the editor's still and JPEG read
/// such a picture (`preview::to_rgb`, `preview::jpeg_matrix`) and the cut
/// lays the transition image in it (`cut_conform::read_overlay`). Left to
/// the composite's own size, an HD master of that kind showed through
/// BT.601 in this window and through BT.709 everywhere else. Not an RGB
/// master's, whose reels the scaler turns into YUV through BT.601.
///
/// **Nor any master whose pictures are RGB (or a palette), stated or not.** The
/// scaler turns those -- its own pictures and every reel's -- into the
/// composite's YUV through BT.601 whatever they say ([`crate::cut::matrices`]
/// swaps nothing for them), and a QuickTime Animation (`qtrle`) master states
/// no matrix: read by its HD size, the window showed BT.601 samples through
/// BT.709, red 255,26,0 where the editor shows 252,1,0. Said to be BT.601,
/// which is what they are: left unstated, the composite was read by its own
/// size, and the window is up to 1280 across -- an RGB master's pictures,
/// and its transition image, through BT.709 in a wide window.
fn masters_matrix(master: &crate::VideoInfo) -> Option<i32> {
    use ff::ffi::AVColorSpace as S;
    if rgb_master(master) {
        return Some(S::AVCOL_SPC_SMPTE170M as i32);
    }
    let (w, h) = (master.width, master.height);
    crate::conform::read_matrix(master.shape.matrix, w, h).or_else(|| {
        (master.shape.matrix != S::AVCOL_SPC_RGB as i32)
            .then(|| crate::conform::read_matrix(S::AVCOL_SPC_UNSPECIFIED as i32, w, h))
            .flatten()
    })
}

/// Whether a master's pictures are RGB or a palette, whose composite
/// [`masters_matrix`] says is BT.601.
fn rgb_master(master: &crate::VideoInfo) -> bool {
    master.shape.pix_fmt >= 0
        && unsafe {
            let d = ff::ffi::av_pix_fmt_desc_get(std::mem::transmute::<i32, ff::ffi::AVPixelFormat>(
                master.shape.pix_fmt,
            ));
            !d.is_null()
                && (*d).flags
                    & (ff::ffi::AV_PIX_FMT_FLAG_RGB | ff::ffi::AV_PIX_FMT_FLAG_PAL) as u64
                    != 0
        }
}

/// One instant of the preview, as a JPEG.
///
/// For scrubbing: each call seeks both clips, which is what makes it answer a
/// pointer being dragged rather than a clock. Playback reads forwards instead
/// -- see [`play`].
///
/// **Each clip's picture is the one [`play`] shows at the same instant**: the
/// one on screen then, read by the same [`Reader`] with the same slack, which
/// is the cutter's rule too. This used to take the picture *nearest* the
/// instant, and an instant past half a picture's interval got the next one:
/// a 29.97 reel after a 25p master showed a different picture from playback
/// and the output at three steps in five, and a 59.94p clip with a 10 second
/// dissolve one picture later at every step before and into the crossing.
pub fn shot(seam: &Seam, window: &Window, t: f64, width: u32) -> Result<Vec<u8>> {
    crate::init()?;
    let shape = seam.shape(width);
    let master = &seam.master().video;
    let look = window.look(t);
    let laid = overlay_of(&seam.transition, shape, master).ok().flatten();
    // The master's frame, as `play` is handed it: only the slack depends on it.
    let fps = master.frame_rate;
    let step = 1.0 / if fps.is_finite() && fps > 0.0 { fps.clamp(1.0, 1000.0) } else { 30.0 };
    // Read from the entry point the picture is decoded from, as
    // `picture_at` reads it: playback's seek margin is three GOPs before the
    // instant, and a still read from there took twice the time. A landing
    // past that entry point is read again from the margin, as `picture_at`
    // reads it again.
    let standing = |src: &Source, at: f64| {
        let half = src.video.frame_duration() / 2.0;
        if let Some(entry) = crate::index::entry_before(&src.points, at, 1e-6) {
            // Across a recorder's seam the reader reads as the cut does (see
            // `Reader::pull` and `Reader::at`), so the still is the picture
            // playback shows there too.
            if let Ok(mut reader) = Reader::on_entry(src, at, shape, master, step) {
                if let Ok(shown) = reader.at(at).map(|f| f.cloned()) {
                    // Nor where the entry point's picture comes after the
                    // instant: a disc's EP map rounds its times down, and an
                    // instant between the rounded time and the picture has
                    // the one before on screen, which this landing never read.
                    let slack = reader.slack;
                    if reader
                        .first
                        .is_some_and(|first| first <= entry.time + half && first <= at + slack)
                    {
                        return shown;
                    }
                }
            }
        }
        Reader::open(src, at, shape, master, step)
            .and_then(|mut reader| Ok(reader.at(at)?.cloned()))
            .ok()
            .flatten()
    };
    let near = look.near.and_then(|at| standing(seam.before, at));
    let far = look.far.and_then(|at| standing(seam.after, at));
    let mut picture = compose(near.as_ref(), far.as_ref(), &look, laid.as_ref(), shape)?;
    in_the_masters_matrix(&mut picture, master);
    crate::preview::jpeg_of(&picture, 1.0, shape.0)
}

/// One clip read forwards, handing over the picture standing at each instant.
///
/// **Read forwards and one picture ahead, never seeked.** The preview asks
/// for a clip at instants that march forwards, so what is needed at each of
/// them is the picture that was on screen then -- which is exactly what
/// reading forwards and holding the last one gives. The one read ahead is
/// what says when the held one stops being the answer. The cutter's own far
/// side works the same way and for the same reason.
struct Reader<'s> {
    /// The clip being read, for [`crate::preview::Landed::mend`].
    src: &'s Source,
    ictx: crate::input::Demux,
    ist: usize,
    decoder: ff::decoder::Video,
    in_tb: f64,
    start_time: f64,
    held: Option<ff::frame::Video>,
    /// Where the stretch of a recorder's joined clip the held picture was
    /// read in stops, where it was read before a seam. See `at`.
    held_stops: Option<f64>,
    ahead: Option<(f64, ff::frame::Video)>,
    rescale: Option<Rescale>,
    shape: (u32, u32),
    /// The recording the joined file takes its shape from; see [`reshape`].
    master: &'s crate::VideoInfo,
    /// How far after an instant a picture may arrive and still be the one on
    /// screen at it. See [`on_screen_slack`].
    slack: f64,
    spent: bool,
    /// Whether the decoder has been told the stream is over. See `pull`.
    drained: bool,
    /// The instant of the first picture decoded, which says whether the seek
    /// landed late. See [`shot`].
    first: Option<f64>,
    /// Where a seek by byte put a program stream, for its entry picture's
    /// time. See [`crate::preview::Landed`].
    landed: Option<crate::preview::Landed>,
    /// Which stretch of a recorder's joined clip the read is in. See `pull`.
    stretches: crate::preview::Stretches<'s>,
    /// Whether the decoder holds anything a drain at a seam would hand back.
    fed: bool,
    /// Pictures a drain at a seam handed back, in order, not yet asked for.
    queued: std::collections::VecDeque<(f64, ff::frame::Video)>,
    /// The H.264 sets in force where the clip is read, for a key picture
    /// that carries none: a clip opened past the IDR the second of two
    /// spliced encodes restated its sets on was decoded by the first one's.
    /// See [`crate::preview::InForce`].
    in_force: crate::preview::InForce,
}

impl<'s> Reader<'s> {
    fn open(
        src: &'s Source,
        from: f64,
        shape: (u32, u32),
        master: &'s crate::VideoInfo,
        step: f64,
    ) -> Result<Reader<'s>> {
        // Back a little, because the picture wanted at the first instant is
        // the one standing there, which may have been coded before it.
        Reader::aimed(src, shape, master, step, |ictx| {
            seek_near(ictx, src, (from - src.seek_margin).max(0.0));
            None
        })
    }

    /// As [`Reader::open`], put on the entry point the picture at `at` is
    /// decoded from rather than a seek margin before it: by its byte where
    /// the index has one, as [`crate::preview::picture_at`] seeks. Whether it
    /// landed there is for the caller to check, by its `first`.
    fn on_entry(
        src: &'s Source,
        at: f64,
        shape: (u32, u32),
        master: &'s crate::VideoInfo,
        step: f64,
    ) -> Result<Reader<'s>> {
        Reader::aimed(src, shape, master, step, |ictx| {
            match crate::index::seek_to_entry(ictx, src, at) {
                // A program stream's entry picture, read from its byte, can
                // come with the next picture's time, and the landing was then
                // judged late and read again from the margin. Only there: a
                // transport stream stamps every picture itself, and a disc's
                // entry point map keeps its times 512 ticks coarse, so its
                // stated time is not the picture's. See
                // [`crate::preview::Landed`].
                Some(entry) if !src.on_a_ts => crate::preview::Landed::at(src, entry),
                Some(_) => None,
                None => {
                    let entry = crate::index::entry_before(&src.points, at, 1e-6);
                    seek_near(ictx, src, entry.map_or(0.0, |p| p.time));
                    None
                }
            }
        })
    }

    fn aimed(
        src: &'s Source,
        shape: (u32, u32),
        master: &'s crate::VideoInfo,
        step: f64,
        seek: impl FnOnce(&mut crate::input::Demux) -> Option<crate::preview::Landed>,
    ) -> Result<Reader<'s>> {
        let mut ictx = crate::input::demux(&src.input.url)?;
        let ist = src.video.stream_index;
        let stream = ictx
            .stream(ist)
            .ok_or_else(|| anyhow!("no video stream in {}", src.path))?;
        let in_tb = f64::from(stream.time_base());
        let params = stream.parameters();
        let in_force = crate::preview::InForce::single(&params, &src.video);
        let decoder = crate::video_decoder(params)?;
        let landed = seek(&mut ictx);
        crate::input::keep_only(&mut ictx, &[ist]);
        Ok(Reader {
            src,
            ictx,
            ist,
            decoder,
            in_tb,
            start_time: src.start_time,
            held: None,
            held_stops: None,
            ahead: None,
            rescale: None,
            shape,
            master,
            slack: on_screen_slack(in_tb, step),
            spent: false,
            drained: false,
            first: None,
            landed,
            stretches: crate::preview::Stretches::new(src),
            fed: false,
            queued: Default::default(),
            in_force,
        })
    }

    /// The picture this clip has on screen at `want`, on its own clock.
    fn at(&mut self, want: f64) -> Result<Option<&ff::frame::Video>> {
        loop {
            match self.ahead.take() {
                Some((t, frame)) if t <= want + self.slack => {
                    self.held = Some(frame);
                    self.held_stops = self.stretch_stops(t);
                }
                Some(pair) => {
                    self.ahead = Some(pair);
                    break;
                }
                None if self.spent => break,
                None => {
                    self.ahead = self.pull()?;
                    if self.ahead.is_none() {
                        self.spent = true;
                    }
                }
            }
        }
        // Nothing on screen yet: the instant asked for is in front of the
        // clip's first picture, which is where a range kept from the head of
        // a recording begins -- its first entry point, read off an index
        // whose rounding need not be the decoder's. Left at nothing, the
        // clip after a plain cut or a fade had no picture at its first
        // instant, and with the clip before already gone the preview took
        // that for the end of the material and stopped at the join. The
        // first picture stands in, as the cut places it at the range's start.
        //
        // And the same past a recorder's seam: where the stretch the held
        // picture was read in stops, the range before the seam ends, and the
        // cut opens the range after it on the next stretch's first picture --
        // which can come a fifth of a second after the seam's time. Held on,
        // the window showed the old stretch's last picture there while its
        // still ([`shot`]) and the output showed the new one.
        let past = self.held_stops.is_some_and(|stop| want >= stop - self.seam_tol());
        if self.held.is_none() || (past && self.ahead.is_some()) {
            return Ok(self.ahead.as_ref().map(|(_, f)| f));
        }
        Ok(self.held.as_ref())
    }

    /// Where the stretch of a recorder's joined clip that a picture shown at
    /// `t` was read in stops: the earlier of the next seam's two instants, as
    /// [`crate::preview`]'s walk ends a stretch. `None` past the last seam,
    /// and on every recording without one.
    fn stretch_stops(&self, t: f64) -> Option<f64> {
        let tol = self.seam_tol();
        self.src
            .joins
            .iter()
            .map(|j| j.ends.min(j.time))
            .filter(|&stop| t < stop - tol)
            .reduce(f64::min)
    }

    /// A thousandth of a frame, as the walk compares a picture with the end
    /// of its stretch.
    fn seam_tol(&self) -> f64 {
        self.src.video.frame_duration() * 1e-3
    }

    /// The next picture of the clip, in the order it is shown.
    ///
    /// **A recorder's seam is read across as the cut reads it**, by the rule
    /// [`crate::preview`]'s walk reads it with: the decoder is drained of the
    /// stretch before the seam, and a picture outside the stretch it was
    /// read in -- one the stretch runs on with past where the range before
    /// the seam ends, or a leading picture of the next stretch stamped before
    /// it begins -- is never handed on, since the cut never writes it. Fed
    /// straight on, a seam window reaching past a recorder seam played
    /// pictures of the old stretch after its end, which neither the output
    /// nor the window's own still ([`shot`]) shows. Nothing changes on a
    /// recording without seams.
    fn pull(&mut self) -> Result<Option<(f64, ff::frame::Video)>> {
        let mut frame = ff::frame::Video::empty();
        loop {
            if let Some(next) = self.queued.pop_front() {
                return Ok(Some(next));
            }
            if self.decoder.receive_frame(&mut frame).is_ok() {
                let Some(pts) = frame.pts() else { continue };
                let t = pts as f64 * self.in_tb - self.start_time;
                if self.stretches.outside(t) {
                    continue;
                }
                self.first.get_or_insert(t);
                let shaped = reshape(&frame, self.shape, Some(self.master), &mut self.rescale)?;
                return Ok(Some((t, shaped)));
            }
            // The decoder told the stream is over, which is not the same as
            // the decoder having nothing left: it is holding its reorder
            // depth and a frame for each of its threads, half a second of
            // the end of a clip. Marking the reader spent here, as this used
            // to, left those unread and the preview stopped short of the
            // clip's end. `FarSide` in the cutter keeps the two apart too.
            let Some((stream, mut packet)) = self.ictx.read_packets().next() else {
                if !self.drained {
                    self.drained = true;
                    self.decoder.send_eof()?;
                    continue;
                }
                return Ok(None);
            };
            if stream.index() != self.ist {
                continue;
            }
            crate::preview::Landed::mend(&mut self.landed, &mut packet, self.src);
            self.in_force.see(&packet);
            let was = self.stretches.clone();
            if self.stretches.reach(packet.position()) && std::mem::take(&mut self.fed) {
                let _ = self.decoder.send_eof();
                while self.decoder.receive_frame(&mut frame).is_ok() {
                    let Some(pts) = frame.pts() else { continue };
                    let t = pts as f64 * self.in_tb - self.start_time;
                    if was.outside(t) {
                        continue;
                    }
                    self.first.get_or_insert(t);
                    let shaped =
                        reshape(&frame, self.shape, Some(self.master), &mut self.rescale)?;
                    self.queued.push_back((t, shaped));
                }
                self.decoder.flush();
            }
            let src = self.src;
            self.in_force.mend(&mut packet, &mut |at, record| {
                Some(crate::preview::sets_before(src, at, record))
            });
            // A packet the decoder will not take is not a reason to stop.
            if self.decoder.send_packet(&packet).is_ok() {
                self.fed = true;
            }
        }
    }
}

/// A timestamp seek to `landing`, or to the very start where nothing before
/// the first entry point could be decoded anyway.
fn seek_near(ictx: &mut crate::input::Demux, src: &Source, landing: f64) {
    let target = if src.points.first().is_none_or(|p| landing <= p.time) {
        crate::input::head_seek(ictx)
    } else {
        ((landing + src.start_time) * f64::from(ff::ffi::AV_TIME_BASE)) as i64
    };
    let _ = ictx.seek(target, ..target);
}

/// How far after an instant of the preview a clip's picture may arrive and
/// still be the one on screen at it: the cutter's own answer
/// (`far_slack` in [`crate::cut`]), so that the preview of a crossing shows
/// the pictures the output will. Shared rather than copied: the copy here
/// gave a fine clock (a transport stream's) a billionth of a second where
/// the cut gives a thousandth of a frame, so a picture stamped within that
/// after an instant was the next picture in the output and the one before
/// in the preview.
fn on_screen_slack(in_tb: f64, step: f64) -> f64 {
    crate::cut::far_slack(in_tb, step)
}

/// Play the preview from `from`, as a stream of pictures.
///
/// Unlike [`crate::preview::play_from`], which hands over whatever the
/// recording's own pictures are timed at, this one walks an even grid: an
/// instant of the *output* is what a crossing is defined at, and the two
/// clips being read have grids of their own that need not agree. `fps` is
/// that grid, and it is the master clip's rate -- the rate the joined file
/// will be written at.
///
/// `pace` sees each instant before it is composited and says what to do with
/// it, which is where waiting and dropping belong: only the caller knows what
/// the clock says.
pub fn play(
    seam: &Seam,
    window: &Window,
    from: f64,
    width: u32,
    fps: f64,
    mut pace: impl FnMut(f64) -> crate::preview::Pace,
    mut show: impl FnMut(f64, Vec<u8>),
) -> Result<()> {
    crate::init()?;
    let shape = seam.shape(width);
    let master = &seam.master().video;
    let laid = overlay_of(&seam.transition, shape, master).ok().flatten();
    // Held to the rates a recording can have (see where `frame_rate` is
    // worked out). The rate arrives from the window, and a huge one made the
    // step too small to move `t` at all: the same instant, composited for
    // ever.
    let step = 1.0 / if fps.is_finite() { fps.clamp(1.0, 1000.0) } else { 30.0 };
    // Opened where each clip is first wanted rather than at the head of the
    // preview: the clip after the join is not on screen for the first half of
    // it, and a reader that started there would decode that half to drop it.
    let mut near =
        Reader::open(seam.before, window.look(from).near.unwrap_or(0.0), shape, master, step)?;
    let mut far: Option<Reader> = None;
    let mut t = from;
    while t < window.seconds - 1e-9 {
        match pace(t) {
            crate::preview::Pace::Stop => break,
            crate::preview::Pace::Skip => {
                t += step;
                continue;
            }
            crate::preview::Pace::Show => {}
        }
        let look = window.look(t);
        let near_frame = match look.near {
            Some(at) => near.at(at)?.cloned(),
            None => None,
        };
        let far_frame = match look.far {
            Some(at) => {
                if far.is_none() {
                    far = Some(Reader::open(seam.after, at, shape, master, step)?);
                }
                far.as_mut().expect("just set").at(at)?.cloned()
            }
            None => None,
        };
        // A picture neither clip can answer for is the end of the material,
        // not an error to stop on: the preview simply runs out.
        if near_frame.is_none() && far_frame.is_none() {
            break;
        }
        let mut picture = compose(
            near_frame.as_ref(),
            far_frame.as_ref(),
            &look,
            laid.as_ref(),
            shape,
        )?;
        in_the_masters_matrix(&mut picture, master);
        show(t, crate::preview::jpeg_of(&picture, 1.0, shape.0)?);
        t += step;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transition::Side;

    /// A `Seam` needs two opened recordings, which a unit test has not got.
    /// The schedule is arithmetic over the times alone, so it is built
    /// directly here.
    fn window_of(kind: Crossing, seconds: f64, lead: f64) -> Window {
        between(kind, seconds, lead, (0.0, 60.0), (10.0, 70.0))
    }

    /// The same, over clips whose bounds are the point of the test.
    fn between(
        kind: Crossing,
        seconds: f64,
        lead: f64,
        (before_in, before_out): (f64, f64),
        (after_in, after_out): (f64, f64),
    ) -> Window {
        let transition = Transition {
            kind,
            seconds,
            easing: Easing::parse("none", "in"),
            ..Default::default()
        };
        let want = transition.takes();
        let room_before = ((before_out - before_in) / 2.0).max(0.0);
        let room_after = ((after_out - after_in) / 2.0).max(0.0);
        let (take_before, take_after) = {
            let (b, a) = (want.0.min(room_before), want.1.min(room_after));
            if kind.overlaps() {
                (b.min(a), b.min(a))
            } else {
                (b, a)
            }
        };
        let overlaps = kind.overlaps();
        let lead_before = lead.min(before_out - take_before - before_in).max(0.0);
        let lead_after = lead.min(after_out - after_in - take_after).max(0.0);
        let at = lead_before;
        let ends = if overlaps {
            at + take_before
        } else {
            at + take_before + take_after
        };
        Window {
            seconds: ends + lead_after,
            at,
            ends,
            before_from: before_out - take_before - lead_before,
            after_in,
            after_at: if overlaps { at } else { at + take_before },
            take_before,
            take_after,
            overlaps,
            kind,
            easing: transition.easing,
        }
    }

    /// An overlapping crossing takes its whole length from both clips, and
    /// the output is that much shorter than the two halves laid end to end.
    /// The same answer [`Transition::takes`] gives the planner.
    #[test]
    fn an_overlap_takes_its_length_from_both_sides() {
        let w = window_of(Crossing::Dissolve, 2.0, 3.0);
        assert_eq!((w.at, w.ends), (3.0, 5.0));
        // The clip before is on screen from three seconds before the crossing
        // and plays right through it, ending where its material ends.
        assert_eq!(w.look(0.0).near, Some(55.0));
        assert_eq!(w.look(4.0).near, Some(59.0));
        // The clip after starts at the crossing's first instant.
        assert_eq!(w.look(3.0).far, Some(10.0));
        assert_eq!(w.look(5.0).far, Some(12.0));
    }

    /// A fade is two halves of the transition's length, one clip in each, and
    /// the output keeps its length.
    #[test]
    fn a_fade_is_two_halves_with_one_clip_in_each() {
        let w = window_of(Crossing::Fade(Shade::Black), 2.0, 3.0);
        assert_eq!((w.at, w.ends), (3.0, 5.0));
        let going_in = w.look(3.5);
        assert!(going_in.far.is_none(), "the clip after is not on screen yet");
        assert!(matches!(going_in.how, How::Tint { mix, .. } if (mix - 0.5).abs() < 1e-6));
        let coming_out = w.look(4.5);
        assert!(coming_out.near.is_none(), "the clip before has finished");
        assert!(matches!(coming_out.how, How::Tint { mix, .. } if (mix - 0.5).abs() < 1e-6));
        // And the clip after picks up at its own first instant.
        assert_eq!(w.look(4.0).far, Some(10.0));
    }

    /// Both ends of every crossing are the plain clips, so nothing shows a
    /// step where the preview begins or ends.
    #[test]
    fn a_crossing_begins_and_ends_on_the_clips_themselves() {
        for kind in [
            Crossing::Dissolve,
            Crossing::Fade(Shade::White),
            Crossing::Wipe(Side::Left),
            Crossing::Slide(Side::Bottom),
        ] {
            let w = window_of(kind, 1.0, 3.0);
            assert_eq!(w.look(0.0).how, How::Plain, "{kind:?} at the head");
            assert!(w.look(0.0).near.is_some(), "{kind:?} at the head");
            let end = w.seconds - 0.001;
            assert_eq!(w.look(end).how, How::Plain, "{kind:?} at the foot");
            assert!(w.look(end).far.is_some(), "{kind:?} at the foot");
        }
    }

    /// A join with no transition on it is still worth looking at: the preview
    /// runs the two clips end to end with nothing between them.
    #[test]
    fn a_plain_cut_previews_as_a_plain_cut() {
        let w = window_of(Crossing::None, 1.0, 3.0);
        assert_eq!((w.at, w.ends), (3.0, 3.0));
        assert_eq!(w.look(2.999).near, Some(59.999));
        assert_eq!(w.look(3.0).far, Some(10.0));
        assert_eq!(w.look(3.0).how, How::Plain);
    }

    /// A still over the crossing comes up and goes down with the crossing's
    /// own ends, over the pair of halves where it is a fade.
    #[test]
    fn the_overlay_spans_the_whole_crossing() {
        let w = window_of(Crossing::Fade(Shade::Black), 4.0, 3.0);
        assert_eq!(w.look(2.999).overlay, 0.0, "before it starts");
        // A quarter in at each end, so the middle half is at full strength.
        assert!((w.look(5.0).overlay - 1.0).abs() < 1e-9, "the middle");
        assert!(w.look(3.5).overlay < 1.0, "still coming up");
        assert!(w.look(6.5).overlay < 1.0, "already going down");
    }

    /// The lead is what is wanted or what there is, whichever is less: a clip
    /// with a second left before the join gets a second of lead, and the
    /// window is shorter rather than starting before the clip does.
    #[test]
    fn the_lead_is_capped_by_the_material() {
        // 60 seconds before the join and 60 after, so three is there to give.
        let roomy = window_of(Crossing::Dissolve, 1.0, 3.0);
        assert_eq!(roomy.at, 3.0);
        // And a clip with barely more than the crossing in front of the join:
        // four seconds of it, two taken by a two second dissolve, which
        // leaves two of lead where three were asked for. The preview begins
        // where the clip begins rather than reaching back into material that
        // is not being written.
        let tight = between(Crossing::Dissolve, 2.0, 3.0, (10.0, 14.0), (0.0, 60.0));
        assert_eq!(tight.at, 2.0, "two seconds of lead is what there is");
        assert_eq!(tight.ends, 4.0);
        assert_eq!(tight.look(0.0).near, Some(10.0), "the clip's own first instant");
    }

    /// The sound is the one clip and then the other, with no gap and no
    /// overlap -- and the handover through a fade is the middle of it, where
    /// one clip's material stops and the other's begins, not the end.
    #[test]
    fn the_sound_hands_over_once() {
        let dissolve = window_of(Crossing::Dissolve, 2.0, 3.0);
        let s = dissolve.sound();
        assert_eq!(s.at, dissolve.ends, "an overlap hands over at the end of it");
        assert_eq!(s.before, (55.0, 60.0), "through the crossing, to its own end");
        assert_eq!(s.after, (12.0, 15.0), "the seconds the crossing already showed");

        let fade = window_of(Crossing::Fade(Shade::Black), 2.0, 3.0);
        let s = fade.sound();
        assert_eq!(s.at, 4.0, "the middle of the fade, where the colour is");
        assert_eq!(s.before, (56.0, 60.0));
        assert_eq!(s.after, (10.0, 14.0), "from its own first instant");

        // No gap and no overlap, whichever it is: what the before side gives
        // up at `at` is exactly where the after side picks up.
        for w in [dissolve, fade, window_of(Crossing::None, 1.0, 3.0)] {
            let s = w.sound();
            assert!(
                (s.before.1 - s.before.0 + (s.after.1 - s.after.0) - w.seconds).abs() < 1e-9,
                "the two stretches come to the whole preview",
            );
        }
    }

    /// The easing is the one the transition was given, read through
    /// [`Easing::at`] and not reimplemented here.
    #[test]
    fn the_easing_is_the_transitions_own() {
        let mut w = window_of(Crossing::Dissolve, 2.0, 3.0);
        w.easing = Easing::parse("quadratic", "in");
        let mid = w.look(4.0);
        let How::Cross { alpha, .. } = mid.how else {
            panic!("a dissolve is a cross")
        };
        assert!(
            (alpha - Easing::parse("quadratic", "in").at(0.5)).abs() < 1e-9,
            "halfway through a quadratic ease-in is {alpha}"
        );
    }

    /// A clip on a millisecond clock has its pictures taken at the instant
    /// they belong to, as the cutter takes them; a transport stream's clock
    /// gets the cutter's thousandth of a frame.
    #[test]
    fn a_coarse_clock_gets_the_cutters_slack() {
        let step = 1001.0 / 30000.0;
        assert!((on_screen_slack(1e-3, step) - 3.5e-3).abs() < 1e-12);
        assert_eq!(on_screen_slack(1.0 / 90_000.0, step), step * 1e-3);
        // Never so wide that the picture after is taken for this one.
        assert!(on_screen_slack(0.1, step) <= step / 3.0);
    }

    /// Both clips at a seam come into the preview at studio levels: a
    /// full-range picture already the preview's size and format is still
    /// converted, and a studio one is left as it is.
    #[test]
    fn the_preview_is_at_studio_levels() {
        crate::init().unwrap();
        let picture = |luma: u8, full: bool| {
            let mut f = ff::frame::Video::new(SHAPE, 16, 8);
            for nth in 0..3 {
                let v = if nth == 0 { luma } else { 128 };
                f.data_mut(nth).iter_mut().for_each(|b| *b = v);
            }
            if full {
                f.set_color_range(ff::util::color::Range::JPEG);
            }
            f
        };
        let mut rescale = None;
        let out = reshape(&picture(0, true), (16, 8), None, &mut rescale).unwrap();
        assert_eq!(out.data(0)[0], 16);
        let out = reshape(&picture(16, false), (16, 8), None, &mut rescale).unwrap();
        assert_eq!(out.data(0)[0], 16);
    }
}
