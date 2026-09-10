//! The headers an advanced-profile VC-1 stream is built out of.
//!
//! A VC-1 elementary stream is a run of BDUs, each introduced by a start
//! code: a sequence header says what the whole stream is, an entry-point
//! header says what may be assumed from the next access point onwards, and a
//! frame header opens each picture. None of the three can be read on its own
//! -- the picture header's shape depends on flags set in the other two --
//! which is why they are parsed together here.
//!
//! Only what this program acts on is read. Everything past the quantizer of
//! a picture header is the business of [`crate::intra`], and pan-scan, which
//! no disc in practice sets, is refused rather than guessed at.

use crate::bits::{unescape, Reader};

/// Start-code suffixes. The prefix is always `00 00 01`.
pub const SEQUENCE: u8 = 0x0F;
pub const ENTRY_POINT: u8 = 0x0E;
pub const FRAME: u8 = 0x0D;
pub const FIELD: u8 = 0x0C;
pub const SLICE: u8 = 0x0B;

/// Split a payload into its BDUs: the type byte and the bytes after it.
pub fn bdus(data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(k, &s)| {
            let end = starts.get(k + 1).copied().unwrap_or(data.len());
            (data[s + 3], &data[s + 4..end])
        })
        .collect()
}

/// The first BDU of the given type, if the payload holds one.
pub fn find(data: &[u8], kind: u8) -> Option<&[u8]> {
    bdus(data)
        .into_iter()
        .find(|&(t, _)| t == kind)
        .map(|(_, p)| p)
}

/// How the quantizer for each picture is arrived at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantizer {
    /// The picture's own index decides, and picks the step table with it.
    Implicit,
    /// Each picture says which of the two step tables it uses.
    Explicit,
    NonUniform,
    Uniform,
}

/// An advanced-profile sequence header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sequence {
    pub level: u8,
    pub frmrtq: u8,
    pub bitrtq: u8,
    pub postproc: bool,
    pub max_width: u32,
    pub max_height: u32,
    /// Whether pictures may carry the flags that repeat a field or a frame.
    /// Called `BROADCAST` in the decoder, `PULLDOWN` in the syntax.
    pub pulldown: bool,
    /// Whether pictures may be coded as anything but progressive frames.
    pub interlace: bool,
    /// Whether each picture carries a frame counter.
    pub tfcntr: bool,
    pub finterp: bool,
    /// Progressive segmented frame: interlaced syntax carrying progressive
    /// pictures, which changes which pulldown flags a picture writes.
    pub psf: bool,
    /// How many leaky buckets the stream declares, if it declares any. The
    /// entry-point header carries one fullness byte per bucket, so the count
    /// has to be known before that can be read.
    pub hrd_buckets: Option<u8>,
    /// Bits the header occupies, encapsulation aside.
    pub bits: usize,
}

/// Read an advanced-profile sequence header, given the bytes after its start
/// code.
pub fn sequence(payload: &[u8]) -> Option<Sequence> {
    let data = unescape(payload);
    let mut r = Reader::new(&data);
    if r.u(2) != 3 {
        return None; // not advanced profile
    }
    let level = r.u(3) as u8;
    if r.u(2) != 1 {
        return None; // colour difference format other than 4:2:0
    }
    let frmrtq = r.u(3) as u8;
    let bitrtq = r.u(5) as u8;
    let postproc = r.bit1();
    let max_width = (r.u(12) + 1) * 2;
    let max_height = (r.u(12) + 1) * 2;
    let pulldown = r.bit1();
    let interlace = r.bit1();
    let tfcntr = r.bit1();
    let finterp = r.bit1();
    r.bit1(); // reserved
    let psf = r.bit1();
    if r.bit1() {
        // display extension: a size, and optionally an aspect ratio, a frame
        // rate and a colour description. None of it changes how a picture is
        // read, but all of it has to be stepped over.
        r.u(14);
        r.u(14);
        if r.bit1() && r.u(4) == 15 {
            r.u(8);
            r.u(8);
        }
        if r.bit1() {
            if r.bit1() {
                r.u(16);
            } else {
                r.u(8);
                r.u(4);
            }
        }
        if r.bit1() {
            r.u(8);
            r.u(8);
            r.u(8);
        }
    }
    let hrd_buckets = r.bit1().then(|| {
        let n = r.u(5) as u8;
        r.u(4);
        r.u(4);
        for _ in 0..n {
            r.u(16);
            r.u(16);
        }
        n
    });
    r.ok().then_some(Sequence {
        level,
        frmrtq,
        bitrtq,
        postproc,
        max_width,
        max_height,
        pulldown,
        interlace,
        tfcntr,
        finterp,
        psf,
        hrd_buckets,
        bits: r.position(),
    })
}

/// An advanced-profile entry-point header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryPoint {
    pub broken_link: bool,
    /// Whether pictures after this point reference anything before it.
    pub closed_entry: bool,
    pub panscan: bool,
    pub refdist: bool,
    pub loopfilter: bool,
    pub fastuvmc: bool,
    pub extended_mv: bool,
    /// Whether, and how, a picture may vary its quantizer between
    /// macroblocks.
    pub dquant: u8,
    /// Whether blocks may be transformed at sizes other than 8x8.
    pub vstransform: bool,
    pub overlap: bool,
    pub quantizer: Quantizer,
    pub coded_size: Option<(u32, u32)>,
    pub extended_dmv: bool,
    pub range_map_y: Option<u8>,
    pub range_map_uv: Option<u8>,
    pub bits: usize,
}

/// Read an entry-point header. The sequence header is needed for the leaky
/// bucket count.
pub fn entry_point(payload: &[u8], seq: &Sequence) -> Option<EntryPoint> {
    let data = unescape(payload);
    let mut r = Reader::new(&data);
    let broken_link = r.bit1();
    let closed_entry = r.bit1();
    let panscan = r.bit1();
    let refdist = r.bit1();
    let loopfilter = r.bit1();
    let fastuvmc = r.bit1();
    let extended_mv = r.bit1();
    let dquant = r.u(2) as u8;
    let vstransform = r.bit1();
    let overlap = r.bit1();
    let quantizer = match r.u(2) {
        0 => Quantizer::Implicit,
        1 => Quantizer::Explicit,
        2 => Quantizer::NonUniform,
        _ => Quantizer::Uniform,
    };
    for _ in 0..seq.hrd_buckets.unwrap_or(0) {
        r.u(8);
    }
    let coded_size = r.bit1().then(|| ((r.u(12) + 1) * 2, (r.u(12) + 1) * 2));
    let extended_dmv = extended_mv && r.bit1();
    let range_map_y = r.bit1().then(|| r.u(3) as u8);
    let range_map_uv = r.bit1().then(|| r.u(3) as u8);
    r.ok().then_some(EntryPoint {
        broken_link,
        closed_entry,
        panscan,
        refdist,
        loopfilter,
        fastuvmc,
        extended_mv,
        dquant,
        vstransform,
        overlap,
        quantizer,
        coded_size,
        extended_dmv,
        range_map_y,
        range_map_uv,
        bits: r.position(),
    })
}

/// How a picture's samples are laid out for coding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coding {
    Progressive,
    /// One picture covering both fields, coded as a frame.
    FrameInterlace,
    /// Two field pictures, coded and carried separately.
    FieldInterlace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    I,
    P,
    B,
    /// An intra picture that is nonetheless never referenced.
    Bi,
    /// A P picture with nothing coded in it at all.
    Skipped,
}

impl Kind {
    /// Whether a later picture may be predicted from this one.
    pub fn reference(self) -> bool {
        !matches!(self, Kind::B | Kind::Bi)
    }
}

/// An advanced-profile picture header, as far as its quantizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Picture {
    pub coding: Coding,
    pub kind: Kind,
    /// The second field's type, where the picture is a pair of fields.
    pub second: Option<Kind>,
    /// Top field first. Meaningful only where the stream sets `PULLDOWN`;
    /// stated as true elsewhere, which is what the decoder assumes.
    pub tff: bool,
    /// Show the first field a second time after the second one.
    pub rff: bool,
    /// Show the whole frame this many extra times. Only a progressive or
    /// segmented-frame stream says this.
    pub rptfrm: u8,
    pub pqindex: u8,
    pub halfqp: bool,
    /// Whether the picture states which quantizer it uses, and which.
    pub uniform_quantizer: bool,
    /// Bits consumed, i.e. where the rest of the picture header begins.
    pub bits: usize,
}

impl Picture {
    /// Whether a later picture may be predicted from this one.
    pub fn reference(&self) -> bool {
        self.kind.reference() && self.second.map_or(true, Kind::reference)
    }

    /// How many fields the picture occupies on the display timeline.
    ///
    /// The pulldown flags are how 24 fps film is carried in a 29.97 stream,
    /// and a timeline built from a single frame duration comes apart without
    /// them.
    pub fn display_fields(&self) -> i64 {
        if self.rff {
            3
        } else {
            2 + 2 * self.rptfrm as i64
        }
    }
}

/// Read a picture header, given the two headers that decide its shape.
pub fn picture(payload: &[u8], seq: &Sequence, entry: &EntryPoint) -> Option<Picture> {
    let data = unescape(payload);
    let mut r = Reader::new(&data);
    let coding = if seq.interlace {
        match r.u012() {
            0 => Coding::Progressive,
            1 => Coding::FrameInterlace,
            _ => Coding::FieldInterlace,
        }
    } else {
        Coding::Progressive
    };
    let (kind, second) = if coding == Coding::FieldInterlace {
        // One field type for each of the two fields, in three bits.
        let fptype = r.u(3);
        let pick = |bit: u32| match (fptype & 4 != 0, fptype & bit != 0) {
            (true, false) => Kind::B,
            (true, true) => Kind::Bi,
            (false, false) => Kind::I,
            (false, true) => Kind::P,
        };
        (pick(2), Some(pick(1)))
    } else {
        let kind = match r.unary(4) {
            0 => Kind::P,
            1 => Kind::B,
            2 => Kind::I,
            3 => Kind::Bi,
            _ => Kind::Skipped,
        };
        (kind, None)
    };
    if seq.tfcntr {
        r.u(8);
    }
    let (mut tff, mut rff, mut rptfrm) = (true, false, 0);
    if seq.pulldown {
        if !seq.interlace || seq.psf {
            rptfrm = r.u(2) as u8;
        } else {
            tff = r.bit1();
            rff = r.bit1();
        }
    }
    if entry.panscan {
        // The syntax here depends on window counts this program has never
        // met on a disc, and a wrong guess would misread everything after
        // it. Say so by refusing rather than by answering wrongly.
        return None;
    }
    if kind == Kind::Skipped {
        // Nothing follows: the picture is a repeat of the last one.
        return r.ok().then_some(Picture {
            coding,
            kind,
            second,
            tff,
            rff,
            rptfrm,
            pqindex: 0,
            halfqp: false,
            uniform_quantizer: false,
            bits: r.position(),
        });
    }
    r.bit1(); // RNDCTRL
    if seq.interlace {
        r.bit1(); // UVSAMP
    }
    match coding {
        Coding::FieldInterlace => {
            if entry.refdist && matches!(kind, Kind::I | Kind::P) {
                if r.u(2) == 3 {
                    r.unary(14);
                }
            }
            if matches!(kind, Kind::B | Kind::Bi) {
                bfraction(&mut r);
            }
        }
        Coding::Progressive => {
            if seq.finterp {
                r.bit1();
            }
            if kind == Kind::B {
                bfraction(&mut r);
            }
        }
        Coding::FrameInterlace => {}
    }
    let pqindex = r.u(5) as u8;
    if pqindex == 0 {
        return None;
    }
    let halfqp = pqindex < 9 && r.bit1();
    let uniform_quantizer = match entry.quantizer {
        Quantizer::Implicit => pqindex < 9,
        Quantizer::Explicit => r.bit1(),
        Quantizer::NonUniform => false,
        Quantizer::Uniform => true,
    };
    if seq.postproc {
        r.u(2);
    }
    r.ok().then_some(Picture {
        coding,
        kind,
        second,
        tff,
        rff,
        rptfrm,
        pqindex,
        halfqp,
        uniform_quantizer,
        bits: r.position(),
    })
}

/// Step over the fraction that says where a B picture sits between its two
/// references: seven values in three bits, the rest in four more.
fn bfraction(r: &mut Reader) {
    if r.u(3) == 7 {
        r.u(4);
    }
}
