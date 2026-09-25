//! Writing an MPEG-2 picture again at a coarser quantiser, without decoding
//! it.
//!
//! This is how a night's recordings are made to fit a disc. The alternative
//! is to decode every picture and encode it again, which costs a great deal
//! of time and throws away everything the recording's own encoder decided:
//! where the GOPs fall, which macroblocks move and where to, how the bits
//! were shared out between a still shot and a pan. None of that has to be
//! decided twice. **The only thing that has to change is how finely the
//! prediction error is written down**, and that is one field per slice, one
//! per macroblock that carries its own, and the coefficients themselves.
//!
//! So the picture comes apart, the coefficients are divided down, and it goes
//! back together. Everything else -- the picture's timestamps, its GOP, its
//! motion vectors, the order of its macroblocks, the intra DC that the
//! blocks after it are coded as differences from -- is the very bits that
//! arrived.
//!
//! ## What it costs
//!
//! **Drift.** The encoder wrote each predicted picture as the difference
//! from a reference it had in front of it. The decoder now rebuilds that
//! reference slightly differently, because the coefficients that built it
//! were rounded here, and the difference is added to rather than corrected.
//! It accumulates along a GOP and is cleared at the next intra picture.
//!
//! That is a real cost and it is the one every fast transrater pays, this
//! program's and the one the reference material here came out of alike:
//! measured against its own source, a disc written this way is at its best
//! on the intra picture that opens a GOP and at its worst on the picture
//! before the next one, and the difference between the two ends is about
//! five decibels at a mild reduction.
//!
//! The alternative is to carry the error forward and subtract it from the
//! next picture, which means keeping decoded references -- an inverse
//! transform and motion compensation, i.e. most of a decoder, and most of
//! the time a decode would have cost.
//!
//! ## What it will not do
//!
//! 4:2:2 and 4:4:4, which no broadcast or Blu-ray recording is; MPEG-1,
//! which has a macroblock stuffing code and no picture coding extension;
//! and the scalable profiles, which put fields in a slice that are not read
//! here. Each of those is an [`Error`] rather than a guess, and a cut that
//! meets one writes the picture through unchanged.

pub mod bits;
mod picture;
pub mod quant;
pub mod scan;
pub mod tables;
mod vlc;

pub use picture::{first_difference, Picture, Scratch, Shape, Squeeze};
pub use quant::Quantiser;

/// Why a picture could not be rewritten.
///
/// None of these is fatal to a cut. The caller writes the picture it was
/// given and goes on to the next one; the run ends up a little larger than
/// it was aiming for and says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No start codes, or no slices among them.
    NotAPicture,
    /// A slice arrived before any picture header said how to read it.
    NoPictureHeader,
    /// A picture coding type that is not I, P or B.
    PictureType,
    /// Chroma that is not 4:2:0.
    Chroma,
    /// One of the scalable profiles, which codes fields this does not read.
    Scalable,
    /// Bits that are no code in the table they were read with, which means
    /// the walk has lost its place.
    BadCode,
    /// A motion type the picture's own structure does not allow.
    MotionType,
    /// A block that runs past its sixty-four coefficients.
    BlockOverrun,
    /// An escape carrying a level the standard forbids.
    BadLevel,
    /// The picture ended in the middle of a macroblock.
    Truncated,
    /// A slice with no macroblocks in it.
    EmptySlice,
    /// The rewrite produced bytes that read back as a different picture --
    /// a false start code among the slice data. Never seen; checked because
    /// the cost of checking is a pass over the bytes and the cost of missing
    /// it is a recording that will not play.
    WroteAStartCode,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Error::NotAPicture => "no coded picture here",
            Error::NoPictureHeader => "a slice with no picture header in front of it",
            Error::PictureType => "a picture that is not I, P or B",
            Error::Chroma => "chroma that is not 4:2:0",
            Error::Scalable => "a scalable profile",
            Error::BadCode => "bits that are no code in the table",
            Error::MotionType => "a motion type this picture cannot have",
            Error::BlockOverrun => "a block of more than sixty-four coefficients",
            Error::BadLevel => "an escape carrying a level the standard forbids",
            Error::Truncated => "the picture stops in the middle of a macroblock",
            Error::EmptySlice => "a slice with no macroblocks",
            Error::WroteAStartCode => "the rewrite produced a false start code",
        };
        f.write_str(s)
    }
}

impl std::error::Error for Error {}

/// How close to the asked-for size the first picture has to land before the
/// search that seeds the run stops looking.
const CLOSE_ENOUGH: f64 = 0.02;

/// How many times that first picture is written to find its size.
const TRIES: usize = 6;

/// How many pictures' worth of bytes of overspend it takes to lift the scale
/// by one more code.
///
/// This is the whole of the rate control and it is deliberately slow. The
/// recording's own encoder decided how to share the bits out between a still
/// shot and a pan, between the picture that opens a GOP and the two that hang
/// off it, and that decision is worth keeping: **what is wanted is the same
/// recording written less finely, not a different recording of the same
/// length.** So the lift stays put and the size is allowed to wander, and
/// only a wander that persists over a second or two of video moves it.
const REACTION: f64 = 5.0;

/// How fast the lift settles on what the material actually needs.
///
/// The standing overspend is what holds the lift up; without this the run
/// would need a permanent debt to stay at the right scale, and that debt is
/// the size it misses its target by. This takes the debt back out over about
/// ten seconds of video and leaves the lift where it was.
const SETTLE: f64 = 0.002;

/// Rewriting a recording's pictures, one after another, at whatever lift
/// it takes to reach a share of the size they arrived as.
pub struct Transrater {
    shape: Shape,
    quantiser: Quantiser,
    scratch: Scratch,
    /// The scale the run has settled on, in codes above what the recording
    /// wrote. Fractional: the part after the point is the share of
    /// macroblocks written one code coarser still.
    base: f64,
    /// What the last picture was actually written at, which is the settled
    /// scale plus whatever the overspend is asking for at the moment.
    last: f64,
    /// Bytes spent beyond what was asked for, so far. Negative is bytes
    /// spared. This is what moves the lift.
    debt: f64,
    /// A running mean of what a picture of this recording costs, which is
    /// what the debt is measured against.
    picture_bytes: f64,
    /// Whether the run has been given its starting scale yet.
    seeded: bool,
    /// What has been through here, for whoever wants to say so afterwards.
    pub written: Tally,
}

/// What a run of pictures came to.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tally {
    pub pictures: u64,
    pub source_bytes: u64,
    pub written_bytes: u64,
    /// Pictures handed back unchanged because they could not be read.
    pub declined: u64,
}

impl Tally {
    /// What the pictures came to as a share of what they arrived as.
    pub fn share(&self) -> f64 {
        if self.source_bytes == 0 {
            1.0
        } else {
            self.written_bytes as f64 / self.source_bytes as f64
        }
    }
}

impl Default for Transrater {
    fn default() -> Self {
        Self::new(Quantiser::default())
    }
}

impl Transrater {
    pub fn new(quantiser: Quantiser) -> Self {
        Self {
            shape: Shape::default(),
            quantiser,
            scratch: Scratch::default(),
            base: 0.0,
            last: 0.0,
            debt: 0.0,
            picture_bytes: 0.0,
            seeded: false,
            written: Tally::default(),
        }
    }

    /// The pressure the run is writing under. See [`squeeze_at`].
    pub fn pressure(&self) -> f64 {
        self.last
    }

    /// What that pressure comes to.
    pub fn squeeze(&self) -> Squeeze {
        squeeze_at(self.last)
    }

    /// Rewrite one coded picture, with a run of them coming to about `share`
    /// of the bytes they arrived as.
    ///
    /// **The share is not applied to this picture.** It is applied to the
    /// recording, and this picture is written at whatever scale the recording
    /// has settled on -- which is the same scale as its neighbours, so the
    /// recording's own sharing out of bits between them survives. A still
    /// shot stays cheap, a pan stays expensive, the picture that opens a GOP
    /// still gets what its GOP needs. Forcing each picture to the same share
    /// of *itself* does the opposite: it spends the most on the pictures that
    /// were already cheap and takes the most from the one every other picture
    /// in the GOP is predicted from. Measured on the reference material at the
    /// same size, that costs four decibels.
    pub fn picture(&mut self, data: &[u8], share: f64) -> Result<Vec<u8>, Error> {
        self.written.pictures += 1;
        self.written.source_bytes += data.len() as u64;
        if !self.seeded {
            self.seeded = true;
            // Where to start. Without this the run writes its first second or
            // two at full size and has to take those bytes back out of what
            // follows, which is a second or two written coarser than the rest
            // for no reason anyone watching could see.
            let want = (data.len() as f64 * share) as usize;
            let mut shape = self.shape;
            if let Ok(picture) = Picture::read(data, &mut shape) {
                self.base = self.seek(&picture, want);
            }
        }
        if self.picture_bytes == 0.0 {
            self.picture_bytes = data.len() as f64;
        }
        // How many bytes of overspend it takes to move the scale by a code.
        let reaction = (self.picture_bytes * REACTION).max(1.0);
        let lift = (self.base + self.debt / reaction).clamp(0.0, PRESSURE_MAX);
        self.last = lift;

        let out = self.one(data, squeeze_at(lift));
        let spent = match out {
            Ok(ref bytes) => bytes.len(),
            Err(_) => {
                self.written.declined += 1;
                data.len()
            }
        };
        self.written.written_bytes += spent as u64;
        self.debt += spent as f64 - data.len() as f64 * share;

        // Hand a little of the overspend over to the settled scale. The lift
        // itself does not move: what was being asked for by the debt is now
        // being asked for by the scale, and the debt that was asking for it
        // is gone. That is what keeps a run from having to stay in debt to
        // stay at the scale its material needs.
        let carry = (lift - self.base) * SETTLE;
        self.base += carry;
        self.debt -= carry * reaction;
        self.picture_bytes = self.picture_bytes * 0.99 + data.len() as f64 * 0.01;
        out
    }

    /// Rewrite one picture at a lift named outright, with no rate control at
    /// all.
    ///
    /// A constant lift is a constant quality: every macroblock of the
    /// recording is written the same number of codes coarser than it arrived,
    /// and what that comes to in bytes is whatever the material makes of it.
    /// What a disc needs is a size, so this is not how a run is written -- but
    /// it is the honest thing to measure a picture's quality against.
    pub fn picture_at(&mut self, data: &[u8], press: Squeeze) -> Result<Vec<u8>, Error> {
        self.written.pictures += 1;
        self.written.source_bytes += data.len() as u64;
        let out = self.one(data, press);
        match out {
            Ok(ref bytes) => self.written.written_bytes += bytes.len() as u64,
            Err(_) => {
                self.written.declined += 1;
                self.written.written_bytes += data.len() as u64;
            }
        }
        out
    }

    /// One picture, under the pressure it is given.
    fn one(&mut self, data: &[u8], press: Squeeze) -> Result<Vec<u8>, Error> {
        let mut shape = self.shape;
        let picture = Picture::read(data, &mut shape);
        // Kept whether or not this picture could be read: the pictures after
        // it carry no sequence header of their own, and read against the one
        // before it a 4:2:2 recording is walked as 4:2:0 -- which can come
        // out as something that parses, and is written back wrong.
        self.shape = shape;
        let picture = picture?;
        if shape.chroma_format != 1 {
            return Err(Error::Chroma);
        }
        let out = picture.write(&self.quantiser, press, &mut self.scratch);
        if start_codes_differ(data, &out) {
            return Err(Error::WroteAStartCode);
        }
        Ok(out)
    }

    /// The lift at which this one picture would come to about `want` bytes.
    ///
    /// Only the first picture of a run is asked. Size falls off roughly as a
    /// constant fraction per code of lift -- a twelfth of it for the first
    /// code on broadcast material -- so the search works on the logarithm of
    /// the size and takes the slope from any two tries it has made.
    fn seek(&mut self, picture: &Picture, want: usize) -> f64 {
        let q = self.quantiser;
        let plain = picture.measure(&q, Squeeze::default(), &mut self.scratch);
        if plain <= want {
            return 0.0;
        }
        let mut slope = 0.5;
        let mut lift = 1.0;
        let mut last: Option<(f64, f64)> = Some((0.0, plain as f64));
        let mut best: Option<(f64, usize)> = None;
        for _ in 0..TRIES {
            let size = picture.measure(&q, squeeze_at(lift), &mut self.scratch);
            if best.is_none_or(|(_, b)| better(size, b, want)) {
                best = Some((lift, size));
            }
            if (size as f64 - want as f64).abs() / want as f64 <= CLOSE_ENOUGH {
                break;
            }
            if let Some((l0, z0)) = last {
                let (dl, dz) = (lift - l0, (size as f64 / z0).ln());
                if dl.abs() > 1e-6 && dz.abs() > 1e-6 {
                    slope = (-dz / dl).clamp(0.02, 2.0);
                }
            }
            last = Some((lift, size as f64));
            let next = (lift + (size as f64 / want as f64).ln() / slope).clamp(0.0, PRESSURE_MAX);
            if (next - lift).abs() < 1e-3 {
                break;
            }
            lift = next;
        }
        best.map_or(0.0, |(l, _)| l)
    }
}

/// The most this will press on a picture. Past here the coefficients are all
/// gone and the scale is at the coarsest code there is; nothing above it
/// means anything.
const PRESSURE_MAX: f64 = 12.0;

/// Where the quantiser scale starts being lifted as well.
///
/// Dropping what is not worth its bits is the better trade and it is taken
/// first, but it only goes so far: past a point the only coefficients left
/// are the ones that are worth keeping, and what is wanted then is to write
/// those less finely. Measured on broadcast material, that point is around
/// here -- by which the pictures are at about half the size they arrived as.
const LIFT_FROM: f64 = 6.0;

/// How many codes the scale lifts by, per unit of pressure past [`LIFT_FROM`].
const LIFT_RATE: f64 = 0.8;

/// What one number of pressure means to a picture.
///
/// Pressure is a single knob because the rate control has to be able to turn
/// something, and it is exponential because that is the shape of what it
/// buys: the first tenth of it takes a twentieth of the size, and the same
/// tenth at the other end takes almost nothing.
fn squeeze_at(pressure: f64) -> Squeeze {
    let pressure = pressure.clamp(0.0, PRESSURE_MAX);
    Squeeze {
        thin: pressure.exp() - 1.0,
        lift: (pressure - LIFT_FROM).max(0.0) * LIFT_RATE,
    }
}

/// Of two sizes, which is the nearer answer for a target. What is left over
/// either way is taken off the next picture, so there is no reason to prefer
/// the side that undershoots.
fn better(size: usize, than: usize, want: usize) -> bool {
    let gap = |n: usize| n.abs_diff(want);
    gap(size) < gap(than)
}

/// Whether the rewrite changed the picture's start codes.
///
/// It must not. Everything outside a slice is copied byte for byte, and the
/// standard's codes cannot spell a start code between them -- but a picture
/// that grew one would be a picture no decoder reads the way this one meant,
/// so it is looked for rather than trusted.
fn start_codes_differ(before: &[u8], after: &[u8]) -> bool {
    let codes = |d: &[u8]| {
        let mut out = Vec::with_capacity(96);
        let mut i = 0;
        while i + 3 < d.len() {
            if d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 {
                out.push(d[i + 3]);
                i += 4;
            } else {
                i += 1;
            }
        }
        out
    };
    codes(before) != codes(after)
}

/// Write a picture back with no lift at all.
///
/// The output has to be the input, byte for byte. That is the test this
/// program has for whether it reads MPEG-2 correctly at all: every field of
/// every macroblock is written again from what was read, so a table with one
/// row wrong, a length counted wrong, a run misplaced -- any of it shows up
/// as bytes that differ. See `tests/run_transrate_tests.sh`.
pub fn rewrite_unchanged(data: &[u8], shape: &mut Shape) -> Result<Vec<u8>, Error> {
    let picture = Picture::read(data, shape)?;
    let q = Quantiser::default();
    let mut scratch = Scratch::default();
    Ok(picture.write(&q, Squeeze::default(), &mut scratch))
}
